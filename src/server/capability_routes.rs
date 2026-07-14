//! Capability Change Proposal routes — submit, decision (approved/rejected).
//! Decision atomically validates content and activates Registry Snapshot.
use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::capability_change::*;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
pub const CAPABILITY_CHANGE_PROPOSE_GRANT: &str = "capability_change.propose";
pub const CAPABILITY_CHANGE_DECIDE_GRANT: &str = "capability_change.decide";
/// Typed error classification for capability route handlers. Each variant
/// maps to a single stable HTTP status and a bounded safe error string.
#[derive(Debug, Clone)]
pub enum CapabilityRouteError {
    InvalidRequest(String),
    AuthNotConfigured,
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
}
impl CapabilityRouteError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidRequest(_) => 400,
            Self::AuthNotConfigured | Self::Unauthorized => 401,
            Self::Forbidden(_) => 403,
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::Internal(_) => 500,
        }
    }
    pub fn safe_error(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::AuthNotConfigured => "capability_auth_not_configured",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Internal(_) => "internal_error",
        }
    }
    /// Include a safe bounded detail string for variants that carry one.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::InvalidRequest(d)
            | Self::Forbidden(d)
            | Self::NotFound(d)
            | Self::Conflict(d)
            | Self::Internal(d) => Some(d.as_str()),
            Self::AuthNotConfigured | Self::Unauthorized => None,
        }
    }
}
impl std::fmt::Display for CapabilityRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::AuthNotConfigured => "capability_auth_not_configured",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Internal(_) => "internal_error",
        };
        write!(f, "{tag}")?;
        if let Some(d) = self.detail() {
            write!(f, ":{d}")?;
        }
        Ok(())
    }
}
impl std::error::Error for CapabilityRouteError {}
/// Sanitise a generic anyhow error — returns a fixed category string for HTTP Fixed category, no original text.
/// 500 responses. NEVER includes original error text, SQL, paths, or tokens.
/// The full error is logged server-side; only a stable category reaches the
/// HTTP body.
pub fn sanitise_error(_err: &anyhow::Error) -> String {
    "internal_error".into()
}
/// Check that `bearer` matches the configured `expected` token. Returns
/// `false` when the token is not configured (fail-closed).
pub fn capability_token_matches(bearer: &str, expected: &Option<String>) -> bool {
    match expected {
        Some(t) => t == bearer,
        None => false,
    }
}
/// Map a `CapabilityRouteError` result into (status_code, body) for the HTTP
/// response. Internal/unexpected errors return `Err` and are rendered as 500.
pub fn map_capability_result(
    result: Result<serde_json::Value>,
) -> std::result::Result<(u16, serde_json::Value), anyhow::Error> {
    match result {
        Ok(v) => Ok((200, v)),
        Err(e) => {
            if let Some(cap_err) = e.downcast_ref::<CapabilityRouteError>() {
                let status = cap_err.http_status();
                let body = serde_json::json!({"ok": false, "error": cap_err.safe_error()});
                Ok((status, body))
            } else {
                Ok((
                    500,
                    serde_json::json!({"ok": false, "error": "internal_error"}),
                ))
            }
        }
    }
}
#[derive(Deserialize)]
pub struct SubmitProposalBody {
    pub target_agent_id: String,
    pub artifact_ref: String,
    pub artifact_digest: String,
    pub manifest_ref: String,
    pub manifest_digest: String,
    pub evidence_ref: String,
    pub evidence_digest: String,
    pub requested_operations: Vec<String>,
    pub risk_summary: String,
}
#[derive(Serialize)]
pub struct SubmitProposalResponse {
    pub proposal_id: String,
    pub status: String,
    pub expected_active_snapshot_id: String,
    pub requested_operations: Vec<String>,
    pub expires_at: String,
}
#[derive(Deserialize)]
pub struct DecisionBody {
    pub decision: String,
    pub artifact_digest: String,
    pub manifest_digest: String,
}
pub fn handle_submit_proposal(
    journal: &JournalStore,
    _gateway: &Gateway,
    body: &Value,
    principal: &str,
    config_agent_id: &AgentId,
) -> Result<SubmitProposalResponse> {
    let input: SubmitProposalBody = serde_json::from_value(body.clone())
        .map_err(|e| CapabilityRouteError::InvalidRequest(format!("{e}")))?;
    // Validate target_agent_id matches the Kernel's configured agent before
    // persisting the proposal. This is re-checked inside the activation tx.
    if !AgentId(input.target_agent_id.clone())
        .0
        .eq_ignore_ascii_case(&config_agent_id.0)
    {
        return Err(CapabilityRouteError::Forbidden("target_agent_mismatch".into()).into());
    }
    // Strictly parse all three digests using the existing Sha256Digest type.
    let _a = Sha256Digest::parse(&input.artifact_digest)
        .map_err(|_| CapabilityRouteError::InvalidRequest("invalid_digest_format".into()))?;
    let _m = Sha256Digest::parse(&input.manifest_digest)
        .map_err(|_| CapabilityRouteError::InvalidRequest("invalid_digest_format".into()))?;
    let _e = Sha256Digest::parse(&input.evidence_digest)
        .map_err(|_| CapabilityRouteError::InvalidRequest("invalid_digest_format".into()))?;
    if input.requested_operations.is_empty() {
        return Err(
            CapabilityRouteError::InvalidRequest("empty_requested_operations".into()).into(),
        );
    }
    let sid = journal
        .current_registry_snapshot_id()
        .map_err(|e| CapabilityRouteError::Internal(format!("{e}")))?;
    let pid = format!("proposal_{}", uuid::Uuid::new_v4().simple());
    let p = CapabilityChangeProposal::new(
        pid.clone(),
        principal.into(),
        AgentId(input.target_agent_id),
        SessionId(String::new()),
        RunId(String::new()),
        input.artifact_ref,
        input.artifact_digest,
        input.manifest_ref,
        input.manifest_digest,
        input.evidence_ref,
        input.evidence_digest,
        input.requested_operations.clone(),
        input.risk_summary,
        sid.clone(),
    );
    journal
        .create_proposal(&p)
        .map_err(|e| CapabilityRouteError::Internal(format!("{e}")))?;
    Ok(SubmitProposalResponse {
        proposal_id: pid,
        status: "PendingApproval".into(),
        expected_active_snapshot_id: sid,
        requested_operations: input.requested_operations,
        expires_at: p.expires_at.to_rfc3339(),
    })
}
pub fn handle_decision(
    journal: &JournalStore,
    _gateway: &Gateway,
    store: &ContentStore,
    proposal_id: &str,
    body: &Value,
    principal: &str,
    config_agent_id: &AgentId,
) -> Result<Value> {
    let proposal = journal
        .load_proposal(proposal_id)?
        .ok_or_else(|| CapabilityRouteError::NotFound("proposal_not_found".into()))?;
    // external.calculator is never allowed through the legacy digest-only
    // decision path. A missing HCR/Approval link therefore fails closed in
    // the trusted handler instead of falling back.
    if proposal.requested_operations == ["external.calculator"] {
        return super::capability_decision::handle(
            journal,
            store,
            proposal_id,
            body,
            config_agent_id,
        );
    }
    let input: DecisionBody = serde_json::from_value(body.clone())
        .map_err(|e| CapabilityRouteError::InvalidRequest(format!("{e}")))?;
    if proposal.status != ProposalStatus::PendingApproval {
        return Err(CapabilityRouteError::Conflict(format!(
            "proposal_not_pending:{:?}",
            proposal.status
        ))
        .into());
    }
    if proposal.submitter_principal_id == principal {
        return Err(CapabilityRouteError::Forbidden("self_decision".into()).into());
    }
    if proposal.expires_at < chrono::Utc::now() {
        // Atomically expire: status → Expired + CapabilityChangeExpired event
        // in a single transaction. No split Update-then-append race.
        journal.expire_proposal_atomic(proposal_id, principal, "expired")?;
        return Err(CapabilityRouteError::Conflict("proposal_expired".into()).into());
    }
    if input.artifact_digest != proposal.artifact_digest {
        return Err(CapabilityRouteError::InvalidRequest("artifact_digest_mismatch".into()).into());
    }
    if input.manifest_digest != proposal.manifest_digest {
        return Err(CapabilityRouteError::InvalidRequest("manifest_digest_mismatch".into()).into());
    }
    match input.decision.as_str() {
        "approved" => {
            // 1. Verify active snapshot matches expected.
            let current_snap_id = journal.current_registry_snapshot_id()?;
            if proposal.expected_active_snapshot_id != current_snap_id {
                return Err(
                    CapabilityRouteError::Conflict("stale_expected_snapshot".into()).into(),
                );
            }
            // 2. Parse digests and re-load + re-hash the three blobs from the
            //    content store. ContentStore::load verifies the digest against
            //    the freshly-read bytes (re-hashes), so any tampering fails here
            //    and the Proposal stays PendingApproval (fail-closed, retryable).
            let art_digest = Sha256Digest::parse(&proposal.artifact_digest)
                .map_err(|_| anyhow!("invalid_artifact_digest_in_proposal"))?;
            let man_digest = Sha256Digest::parse(&proposal.manifest_digest)
                .map_err(|_| anyhow!("invalid_manifest_digest_in_proposal"))?;
            let ev_digest = Sha256Digest::parse(&proposal.evidence_digest)
                .map_err(|_| anyhow!("invalid_evidence_digest_in_proposal"))?;
            let _artifact_bytes = store
                .load(&art_digest)
                .map_err(|e| anyhow!("artifact_verification_failed:{e}"))?;
            let manifest_bytes = store
                .load(&man_digest)
                .map_err(|e| anyhow!("manifest_verification_failed:{e}"))?;
            let _evidence_bytes = store
                .load(&ev_digest)
                .map_err(|e| anyhow!("evidence_verification_failed:{e}"))?;
            // 3. Parse the manifest bytes using the EXISTING HarnessManifest
            //    parser (serde) and run the EXISTING full validator.
            //    validate_all() covers: endpoint loopback, operation_name
            //    `external.` prefix, artifact_digest format, protocol_version,
            //    and both input/output JSON schemas.
            let manifest: crate::harness::manifest::HarnessManifest =
                serde_json::from_slice(&manifest_bytes)
                    .map_err(|e| anyhow!("manifest_parse_failed:{e}"))?;
            manifest
                .validate_all()
                .map_err(|e| anyhow!("manifest_validation_failed:{e}"))?;
            // Recompute the manifest content digest and confirm it matches the
            // stored manifest_id — a tampered manifest fails closed here too.
            let recomputed_manifest_id = manifest
                .compute_manifest_id()
                .map_err(|e| anyhow!("manifest_id_recompute_failed:{e}"))?;
            if recomputed_manifest_id != manifest.manifest_id {
                return Err(
                    CapabilityRouteError::InvalidRequest("manifest_id_mismatch".into()).into(),
                );
            }
            // 4. Bind the manifest artifact_digest to the proposal artifact_digest.
            if manifest.artifact_digest != proposal.artifact_digest {
                return Err(CapabilityRouteError::InvalidRequest(
                    "manifest_artifact_digest_mismatch".into(),
                )
                .into());
            }
            // 5. Extract the manifest operation set and require exact set
            //    equality with proposal.requested_operations. No missing,
            //    no extra, no duplicates (set semantics; order-independent).
            let mut manifest_ops: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            if !manifest.operation_name.is_empty() {
                if !manifest_ops.insert(manifest.operation_name.clone()) {
                    return Err(CapabilityRouteError::InvalidRequest(
                        "duplicate_manifest_operation".into(),
                    )
                    .into());
                }
            }
            let proposal_ops: std::collections::BTreeSet<String> =
                proposal.requested_operations.iter().cloned().collect();
            if proposal_ops.len() != proposal.requested_operations.len() {
                return Err(CapabilityRouteError::InvalidRequest(
                    "duplicate_proposal_operation".into(),
                )
                .into());
            }
            if manifest_ops != proposal_ops {
                // Distinguish the common cases for clearer error categories.
                let missing: Vec<_> = proposal_ops.difference(&manifest_ops).cloned().collect();
                let extra: Vec<_> = manifest_ops.difference(&proposal_ops).cloned().collect();
                if !missing.is_empty() {
                    return Err(CapabilityRouteError::InvalidRequest(format!(
                        "manifest_operation_missing:{missing:?}"
                    ))
                    .into());
                }
                return Err(CapabilityRouteError::InvalidRequest(format!(
                    "manifest_operation_extra:{extra:?}"
                ))
                .into());
            }
            // 6. Namespace + conflict guards. Only external.* is permitted;
            //    builtin.* and development.* are rejected. Empty/illegal names
            //    are caught by validate_operation_name above.
            for op in &proposal.requested_operations {
                if op.starts_with("builtin.") {
                    return Err(CapabilityRouteError::Forbidden(format!(
                        "builtin_namespace_not_allowed:{op}"
                    ))
                    .into());
                }
                if op.starts_with("development.") {
                    return Err(CapabilityRouteError::Forbidden(format!(
                        "development_namespace_not_allowed:{op}"
                    ))
                    .into());
                }
            }
            // 7. Determine if this is a new operation or a schema-only upgrade.
            let current_snap = journal.load_registry_snapshot(&current_snap_id)?;
            let all_ops_exist = proposal
                .requested_operations
                .iter()
                .all(|op| current_snap.lookup(op).is_some());
            let no_ops_exist = proposal
                .requested_operations
                .iter()
                .all(|op| current_snap.lookup(op).is_none());
            if no_ops_exist {
                // ── Standard create path ──
                let risk =
                    crate::domain::operation::coding_operation_risk(&manifest.operation_name)
                        .unwrap_or(crate::registry::snapshot::Risk::Write);
                let spec = crate::registry::snapshot::OperationSpec {
                    name: manifest.operation_name.clone(),
                    risk,
                    description: manifest.description.clone(),
                    parameters: manifest.input_schema.clone(),
                    idempotent: manifest.idempotent,
                    binding_kind: crate::registry::snapshot::BindingKind::External,
                    binding_key: manifest.manifest_id.clone(),
                };
                let mut new_specs: Vec<crate::registry::snapshot::OperationSpec> =
                    current_snap.operations.iter().cloned().collect();
                new_specs.push(spec);
                let decision_id = format!("activation:{}", proposal_id);
                let new_snapshot_id = journal.activate_proposal_atomic(
                    &proposal,
                    principal,
                    new_specs,
                    &proposal.expected_active_snapshot_id,
                    &decision_id,
                    Some(&manifest),
                    config_agent_id,
                )?;
                return Ok(json!({"proposal_id": proposal_id, "status": "Activated",
                    "previous_snapshot_id": proposal.expected_active_snapshot_id,
                    "activated_snapshot_id": new_snapshot_id}));
            }
            if all_ops_exist {
                // ── Schema-only upgrade path ──
                let decision_id = format!("schema_upgrade:{}", proposal_id);
                let new_snapshot_id = journal.activate_schema_upgrade_atomic(
                    &proposal,
                    principal,
                    &decision_id,
                    &manifest,
                    config_agent_id,
                )?;
                return Ok(json!({"proposal_id": proposal_id, "status": "Activated",
                    "previous_snapshot_id": proposal.expected_active_snapshot_id,
                    "activated_snapshot_id": new_snapshot_id}));
            }
            // Mixed: some exist, some don't — not allowed.
            return Err(CapabilityRouteError::Conflict(
                "mixed_create_and_upgrade_not_supported".into(),
            )
            .into());
        }
        "rejected" => {
            journal.reject_proposal_atomic(proposal_id, principal, "rejected")?;
            Ok(json!({"proposal_id": proposal_id, "status": "Rejected"}))
        }
        _ => return Err(CapabilityRouteError::InvalidRequest("invalid_decision".into()).into()),
    }
}

/// Handle a read-only GET request for a capability change proposal.
/// Returns the proposal's authoritative fields needed by the Feishu approval
/// adapter (artifact_digest, manifest_digest, manifest_ref as manifest_id,
/// operation_name, status, endpoint).
///
/// The `manifest_ref` stored on the proposal is returned as `manifest_id`;
/// callers that need the full manifest should use the `load_harness_manifest`
/// journal method.
pub fn handle_get_proposal(
    journal: &JournalStore,
    _store: &ContentStore,
    proposal_id: &str,
) -> Result<Value> {
    let proposal = journal
        .load_proposal(proposal_id)?
        .ok_or_else(|| CapabilityRouteError::NotFound("proposal_not_found".into()))?;

    // Try to load manifest for endpoint info (best-effort).
    let manifest_id = proposal.manifest_ref.clone();
    let manifest_info = journal.load_harness_manifest(&manifest_id).ok().flatten();
    let approval = journal.load_capability_approval_by_proposal(proposal_id)?;
    let origin_context = journal.load_proposal_origin_context(proposal_id)?;
    let approval_json = approval.map(|approval| {
        let (origin_channel, origin_conversation_kind) = origin_context
            .clone()
            .unwrap_or_else(|| ("unknown".into(), "unknown".into()));
        json!({
            "approval_id": approval.approval_id,
            "principal_id": approval.owner_principal_id,
            "expected_source_snapshot_id": approval.source_registry_snapshot_id,
            "candidate_digest": approval.candidate_digest,
            "artifact_digest": approval.artifact_digest,
            "manifest_digest": approval.manifest_digest,
            "decision_nonce": approval.decision_nonce,
            "expires_at": approval.expires_at.to_rfc3339(),
            "status": approval.status,
            "origin_channel": origin_channel,
            "origin_conversation_kind": origin_conversation_kind,
        })
    });

    let resp = json!({
        "proposal_id": proposal.proposal_id,
        "status": proposal.status,
        "operation_name": proposal.requested_operations.first().unwrap_or(&"".to_string()),
        "manifest_id": manifest_id,
        "artifact_digest": proposal.artifact_digest,
        "manifest_digest": proposal.manifest_digest,
        "risk": proposal.risk_summary,
        "endpoint": manifest_info.as_ref().map_or("", |m| m.endpoint.as_str()),
        "expected_active_snapshot_id": proposal.expected_active_snapshot_id,
        "created_at": proposal.created_at.to_rfc3339(),
        "expires_at": proposal.expires_at.to_rfc3339(),
        "decided_at": proposal.decided_at.map(|d| d.to_rfc3339()),
        "decision_reason": proposal.decision_reason,
        "approval": approval_json,
    });
    Ok(resp)
}
