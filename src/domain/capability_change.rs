//! Capability Change Proposal — the Kernel's authoritative record of a proposed
//! external harness addition. Submitted by external development systems, approved
//! by human operators, and activated via the existing Registry Snapshot machinery.
//!
//! The Kernel does NOT build, test, or develop the harness — it only validates
//! the immutable digests and orchestrates the trust-to-activation pipeline.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{AgentId, RunId, SessionId};

/// A capability change proposal. Created by external development systems,
/// approved by human operators, and activated by the Kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityChangeProposal {
    pub proposal_id: String,
    pub submitter_principal_id: String,
    pub target_agent_id: AgentId,
    pub origin_session_id: SessionId,
    pub origin_run_id: RunId,

    pub artifact_ref: String,
    pub artifact_digest: String,
    pub manifest_ref: String,
    pub manifest_digest: String,
    pub evidence_ref: String,
    pub evidence_digest: String,

    pub requested_operations: Vec<String>,
    pub risk_summary: String,

    pub expected_active_snapshot_id: String,

    pub status: ProposalStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,

    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,

    pub activated_snapshot_id: Option<String>,
    pub activation_error: Option<String>,
}

impl CapabilityChangeProposal {
    pub fn new(
        proposal_id: String,
        submitter: String,
        target_agent: AgentId,
        session: SessionId,
        run: RunId,
        artifact_ref: String,
        artifact_digest: String,
        manifest_ref: String,
        manifest_digest: String,
        evidence_ref: String,
        evidence_digest: String,
        operations: Vec<String>,
        risk: String,
        expected_snapshot: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            proposal_id,
            submitter_principal_id: submitter,
            target_agent_id: target_agent,
            origin_session_id: session,
            origin_run_id: run,
            artifact_ref,
            artifact_digest,
            manifest_ref,
            manifest_digest,
            evidence_ref,
            evidence_digest,
            requested_operations: operations,
            risk_summary: risk,
            expected_active_snapshot_id: expected_snapshot,
            status: ProposalStatus::PendingApproval,
            created_at: now,
            expires_at: now + chrono::Duration::days(30),
            decided_at: None,
            decided_by: None,
            decision_reason: None,
            activated_snapshot_id: None,
            activation_error: None,
        }
    }
}

/// Lifecycle status for a CapabilityChangeProposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProposalStatus {
    PendingApproval,
    Approved,
    Rejected,
    Activated,
    ActivationFailed,
    Expired,
}
