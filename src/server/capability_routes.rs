//! Capability Change Proposal HTTP routes — submit, approve, reject.

use crate::domain::capability_change::*;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const CAPABILITY_CHANGE_PROPOSE_GRANT: &str = "capability_change.propose";
pub const CAPABILITY_CHANGE_APPROVE_GRANT: &str = "capability_change.approve";
pub const CAPABILITY_CHANGE_REJECT_GRANT: &str = "capability_change.reject";
pub const CAPABILITY_CHANGE_ACTIVATE_GRANT: &str = "capability_change.activate";

#[derive(Deserialize)]
pub struct SubmitProposalBody {
    pub target_agent_id: String,
    pub artifact_ref: String, pub artifact_digest: String,
    pub manifest_ref: String, pub manifest_digest: String,
    pub evidence_ref: String, pub evidence_digest: String,
    pub requested_operations: Vec<String>,
    pub risk_summary: String,
}

#[derive(Serialize)]
pub struct SubmitProposalResponse {
    pub proposal_id: String, pub status: String,
    pub expected_active_snapshot_id: String,
    pub requested_operations: Vec<String>,
    pub expires_at: String,
}

pub fn handle_submit_proposal(
    journal: &JournalStore, gateway: &Gateway,
    body: &Value, principal: &str,
) -> Result<SubmitProposalResponse> {
    if !gateway.has_grant(principal, CAPABILITY_CHANGE_PROPOSE_GRANT) {
        bail!("capability_change_propose_denied: missing grant");
    }
    let input: SubmitProposalBody = serde_json::from_value(body.clone())
        .map_err(|e| anyhow!("invalid_proposal_body: {e}"))?;
    for (name, val) in [("artifact_digest", &input.artifact_digest),
                        ("manifest_digest", &input.manifest_digest),
                        ("evidence_digest", &input.evidence_digest)] {
        if !val.starts_with("sha256:") || val.len() != 71 {
            bail!("invalid_digest_format:{name}");
        }
    }
    if input.requested_operations.is_empty() {
        bail!("empty_requested_operations");
    }
    let active_snapshot_id = journal.current_registry_snapshot_id()?;
    let proposal_id = format!("proposal_{}", uuid::Uuid::new_v4().simple());
    let proposal = CapabilityChangeProposal::new(
        proposal_id.clone(), principal.to_string(),
        AgentId(input.target_agent_id), SessionId(String::new()), RunId(String::new()),
        input.artifact_ref, input.artifact_digest,
        input.manifest_ref, input.manifest_digest,
        input.evidence_ref, input.evidence_digest,
        input.requested_operations.clone(), input.risk_summary,
        active_snapshot_id.clone(),
    );
    let pid = journal.create_proposal(&proposal)?;
    Ok(SubmitProposalResponse {
        proposal_id: pid, status: "PendingApproval".into(),
        expected_active_snapshot_id: active_snapshot_id,
        requested_operations: input.requested_operations,
        expires_at: proposal.expires_at.to_rfc3339(),
    })
}

pub fn handle_approve_proposal(
    journal: &JournalStore, gateway: &Gateway,
    proposal_id: &str, principal: &str,
) -> Result<Value> {
    if !gateway.has_grant(principal, CAPABILITY_CHANGE_APPROVE_GRANT) {
        bail!("capability_change_approve_denied: missing grant");
    }
    let proposal = journal.load_proposal(proposal_id)?
        .ok_or_else(|| anyhow::anyhow!("proposal_not_found"))?;
    if proposal.status != ProposalStatus::PendingApproval {
        bail!("proposal_not_pending: {:?}", proposal.status);
    }
    if proposal.submitter_principal_id == principal {
        bail!("submitter_cannot_approve_own_proposal");
    }
    let changed = journal.decide_proposal(
        proposal_id, &[ProposalStatus::PendingApproval],
        ProposalStatus::Approved, principal, "approved",
        None, None,
    )?;
    if !changed { bail!("proposal_concurrent_modification"); }
    journal.append_event(
        JournalEventKind::CapabilityChangeApproved,
        Some(&proposal.origin_run_id), Some(&proposal.origin_session_id),
        Some(proposal_id),
        json!({"proposal_id": proposal_id, "decided_by": principal,
               "expected_snapshot_id": proposal.expected_active_snapshot_id}),
    )?;
    Ok(json!({"proposal_id": proposal_id, "status": "Approved",
              "expected_active_snapshot_id": proposal.expected_active_snapshot_id}))
}

pub fn handle_reject_proposal(
    journal: &JournalStore, gateway: &Gateway,
    proposal_id: &str, principal: &str, reason: &str,
) -> Result<Value> {
    if !gateway.has_grant(principal, CAPABILITY_CHANGE_REJECT_GRANT) {
        bail!("capability_change_reject_denied: missing grant");
    }
    let proposal = journal.load_proposal(proposal_id)?
        .ok_or_else(|| anyhow::anyhow!("proposal_not_found"))?;
    if proposal.status != ProposalStatus::PendingApproval {
        bail!("proposal_not_pending: {:?}", proposal.status);
    }
    let changed = journal.decide_proposal(
        proposal_id, &[ProposalStatus::PendingApproval],
        ProposalStatus::Rejected, principal, reason, None, None,
    )?;
    if !changed { bail!("proposal_concurrent_modification"); }
    journal.append_event(
        JournalEventKind::CapabilityChangeRejected,
        Some(&proposal.origin_run_id), Some(&proposal.origin_session_id),
        Some(proposal_id),
        json!({"proposal_id": proposal_id, "decided_by": principal, "reason": reason}),
    )?;
    Ok(json!({"proposal_id": proposal_id, "status": "Rejected"}))
}
