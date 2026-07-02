//! Capability Change Proposal routes — submit, decision (approved/rejected).
//! Decision atomically validates content and activates Registry Snapshot.

use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::capability_change::*;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::{anyhow, bail, Result};
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
        write!(f, "{tag}")
    }
}

impl std::error::Error for CapabilityRouteError {}

/// Sanitise a generic anyhow error into a bounded safe string for HTTP 500
/// responses. Never leaks SQL, paths, tokens, or backtraces.
pub fn sanitise_error(err: &anyhow::Error) -> String {
    let msg = err.to_string();
    let safe: String = msg
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();
    if safe.is_empty() {
        "error".into()
    } else {
        safe
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
    let input: SubmitProposalBody =
        serde_json::from_value(body.clone()).map_err(|e| anyhow!("invalid_proposal_body: {e}"))?;

    // Validate target_agent_id matches the Kernel's configured agent before
    // persisting the proposal. This is re-checked inside the activation tx.
    if !AgentId(input.target_agent_id.clone())
        .0
        .eq_ignore_ascii_case(&config_agent_id.0)
    {
        return Err(CapabilityRouteError::Forbidden("target_agent_mismatch".into()).into());
    }
    for d in [
        &input.artifact_digest,
        &input.manifest_digest,
        &input.evidence_digest,
    ] {
        if !d.starts_with("sha256:") || d.len() != 71 {
            bail!("invalid_digest_format");
        }
    }
    if input.requested_operations.is_empty() {
        bail!("empty_requested_operations");
    }
    let sid = journal.current_registry_snapshot_id()?;
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
    journal.create_proposal(&p)?;
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
    let input: DecisionBody =
        serde_json::from_value(body.clone()).map_err(|e| anyhow!("invalid_decision_body: {e}"))?;
    let proposal = journal
        .load_proposal(proposal_id)?
        .ok_or_else(|| anyhow!("proposal_not_found"))?;
    if proposal.status != ProposalStatus::PendingApproval {
        bail!("proposal_not_pending: {:?}", proposal.status);
    }
    if proposal.submitter_principal_id == principal {
        bail!("submitter_cannot_decide_own_proposal");
    }
    if proposal.expires_at < chrono::Utc::now() {
        // Atomically expire: status → Expired + CapabilityChangeExpired event
        // in a single transaction. No split Update-then-append race.
        journal.expire_proposal_atomic(proposal_id, principal, "expired")?;
        bail!("proposal_expired");
    }
    if input.artifact_digest != proposal.artifact_digest {
        bail!("artifact_digest_mismatch");
    }
    if input.manifest_digest != proposal.manifest_digest {
        bail!("manifest_digest_mismatch");
    }

    match input.decision.as_str() {
        "approved" => {
            // 1. Verify active snapshot matches expected.
            let current_snap_id = journal.current_registry_snapshot_id()?;
            if proposal.expected_active_snapshot_id != current_snap_id {
                bail!("stale_expected_snapshot");
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
                bail!("manifest_id_mismatch");
            }

            // 4. Bind the manifest artifact_digest to the proposal artifact_digest.
            if manifest.artifact_digest != proposal.artifact_digest {
                bail!("manifest_artifact_digest_mismatch");
            }

            // 5. Extract the manifest operation set and require exact set
            //    equality with proposal.requested_operations. No missing,
            //    no extra, no duplicates (set semantics; order-independent).
            let mut manifest_ops: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            if !manifest.operation_name.is_empty() {
                if !manifest_ops.insert(manifest.operation_name.clone()) {
                    bail!("duplicate_manifest_operation");
                }
            }
            let proposal_ops: std::collections::BTreeSet<String> =
                proposal.requested_operations.iter().cloned().collect();
            if proposal_ops.len() != proposal.requested_operations.len() {
                bail!("duplicate_proposal_operation");
            }
            if manifest_ops != proposal_ops {
                // Distinguish the common cases for clearer error categories.
                let missing: Vec<_> = proposal_ops.difference(&manifest_ops).cloned().collect();
                let extra: Vec<_> = manifest_ops.difference(&proposal_ops).cloned().collect();
                if !missing.is_empty() {
                    bail!("manifest_operation_missing:{missing:?}");
                }
                bail!("manifest_operation_extra:{extra:?}");
            }

            // 6. Namespace + conflict guards. Only external.* is permitted;
            //    builtin.* and development.* are rejected. Empty/illegal names
            //    are caught by validate_operation_name above.
            for op in &proposal.requested_operations {
                if op.starts_with("builtin.") {
                    bail!("builtin_namespace_not_allowed:{op}");
                }
                if op.starts_with("development.") {
                    bail!("development_namespace_not_allowed:{op}");
                }
            }

            // 7. Reject activation if any requested operation already exists in
            //    the current active snapshot (no silent overwrite).
            let current_snap = journal.load_registry_snapshot(&current_snap_id)?;
            for op in &proposal.requested_operations {
                if current_snap.lookup(op).is_some() {
                    bail!("existing_operation_conflict:{op}");
                }
            }

            // 8. Build the new operation specs from the verified manifest and
            //    activate atomically. Manifest registration, Registry Snapshot
            //    creation, CAS state update, proposal status, and all journal
            //    events happen INSIDE a single BEGIN IMMEDIATE transaction via
            //    activate_proposal_atomic. Any failure rolls back everything —
            //    no manifest row, no HarnessManifestRegistered event, no Snapshot,
            //    no status change, no terminal events.
            let mut new_specs: Vec<crate::registry::snapshot::OperationSpec> =
                current_snap.operations.iter().cloned().collect();
            new_specs.push(crate::registry::snapshot::OperationSpec {
                name: manifest.operation_name.clone(),
                risk: crate::registry::snapshot::Risk::ReadOnly,
                description: manifest.description.clone(),
                parameters: manifest.input_schema.clone(),
                idempotent: manifest.idempotent,
                binding_kind: crate::registry::snapshot::BindingKind::External,
                binding_key: manifest.manifest_id.clone(),
            });
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

            Ok(json!({"proposal_id": proposal_id, "status": "Activated",
                      "previous_snapshot_id": proposal.expected_active_snapshot_id,
                      "activated_snapshot_id": new_snapshot_id}))
        }
        "rejected" => {
            journal.reject_proposal_atomic(proposal_id, principal, "rejected")?;
            Ok(json!({"proposal_id": proposal_id, "status": "Rejected"}))
        }
        _ => bail!("invalid_decision: must be approved or rejected"),
    }
}
