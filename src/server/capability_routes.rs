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
) -> Result<SubmitProposalResponse> {
    let input: SubmitProposalBody =
        serde_json::from_value(body.clone()).map_err(|e| anyhow!("invalid_proposal_body: {e}"))?;
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
    proposal_id: &str,
    body: &Value,
    principal: &str,
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
        journal.decide_proposal(
            proposal_id,
            &[ProposalStatus::PendingApproval],
            ProposalStatus::Expired,
            principal,
            "expired",
            None,
            None,
        )?;
        journal.append_event(
            JournalEventKind::CapabilityChangeRejected,
            Some(&proposal.origin_run_id),
            Some(&proposal.origin_session_id),
            Some(proposal_id),
            json!({"proposal_id": proposal_id, "reason": "expired"}),
        )?;
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
            // 2. Parse and verify digests.
            let art_digest = Sha256Digest::parse(&proposal.artifact_digest)
                .map_err(|_| anyhow!("invalid_artifact_digest_in_proposal"))?;
            let man_digest = Sha256Digest::parse(&proposal.manifest_digest)
                .map_err(|_| anyhow!("invalid_manifest_digest_in_proposal"))?;
            // 3. Verify content via store (reads + re-hashes).
            let store = ContentStore::new(std::path::PathBuf::from("/tmp/dev-harness-artifacts"));
            let _artifact_bytes = store.load(&art_digest)
                .map_err(|e| anyhow!("artifact_verification_failed:{e}"))?;
            let _manifest_bytes = store.load(&man_digest)
                .map_err(|e| anyhow!("manifest_verification_failed:{e}"))?;
            // Note: evidence verification and manifest parsing are stubs for now.
            // Full implementation requires the harness manifest parser integration.

            // 4. Real Registry Snapshot activation.
            let current_snap = journal.load_registry_snapshot(&current_snap_id)?;
            let new_op = crate::registry::snapshot::OperationSpec {
                name: proposal.requested_operations.get(0).cloned().unwrap_or_default(),
                risk: crate::registry::snapshot::Risk::ReadOnly,
                description: proposal.risk_summary.clone(),
                parameters: serde_json::json!({"type":"object"}),
                idempotent: true,
                binding_kind: crate::registry::snapshot::BindingKind::External,
                binding_key: format!("manifest_{}", proposal_id),
            };
            let mut new_specs: Vec<crate::registry::snapshot::OperationSpec> =
                current_snap.operations.clone();
            new_specs.push(new_op);
            let new_snapshot = journal.create_registry_snapshot(new_specs)?;
            let new_snapshot_id = new_snapshot.snapshot_id.clone();
            // 5. CAS activate via the authoritative activation function.
            let decision_id = format!("activation:{}", proposal_id);
            journal.activate_snapshot_transactional(
                &proposal.expected_active_snapshot_id, &new_snapshot_id,
                &decision_id, "capability_activation",
            )?;
            // 6. Mark proposal Activated.
            journal.decide_proposal(
                proposal_id, &[ProposalStatus::PendingApproval],
                ProposalStatus::Activated, principal, "activated",
                Some(&new_snapshot_id), None,
            )?;
            journal.append_event(
                JournalEventKind::CapabilityChangeActivated,
                Some(&proposal.origin_run_id), Some(&proposal.origin_session_id),
                Some(proposal_id),
                json!({"proposal_id": proposal_id, "decided_by": principal,
                       "previous_snapshot_id": proposal.expected_active_snapshot_id,
                       "new_snapshot_id": new_snapshot_id}),
            )?;
            Ok(json!({"proposal_id": proposal_id, "status": "Activated",
                      "previous_snapshot_id": proposal.expected_active_snapshot_id,
                      "activated_snapshot_id": new_snapshot_id}))
        }
        "rejected" => {
            journal.decide_proposal(
                proposal_id,
                &[ProposalStatus::PendingApproval],
                ProposalStatus::Rejected,
                principal,
                "rejected",
                None,
                None,
            )?;
            journal.append_event(
                JournalEventKind::CapabilityChangeRejected,
                Some(&proposal.origin_run_id),
                Some(&proposal.origin_session_id),
                Some(proposal_id),
                json!({"proposal_id": proposal_id, "decided_by": principal}),
            )?;
            Ok(json!({"proposal_id": proposal_id, "status": "Rejected"}))
        }
        _ => bail!("invalid_decision: must be approved or rejected"),
    }
}
