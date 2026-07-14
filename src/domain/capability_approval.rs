//! Kernel-owned identity for one human decision on a trusted capability Proposal.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityApprovalStatus {
    Pending,
    Approved,
    Rejected,
    ActivationFailed,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityApproval {
    pub approval_id: String,
    pub proposal_id: String,
    pub owner_principal_id: String,
    pub source_registry_snapshot_id: String,
    pub candidate_digest: String,
    pub artifact_digest: String,
    pub manifest_digest: String,
    pub decision_nonce: String,
    pub status: CapabilityApprovalStatus,
    pub decision_id: Option<String>,
    pub decision_payload_digest: Option<String>,
    pub decision_result_json: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub activated_snapshot_id: Option<String>,
    pub host_deployment_id: Option<String>,
    pub activation_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Durable identity/result used to distinguish an idempotent retry from a
/// conflicting second decision.  Pending Approvals have no recorded result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalReplayIdentity {
    pub approval_id: String,
    pub proposal_id: String,
    pub decision_nonce: String,
    pub status: CapabilityApprovalStatus,
    pub decision_id: Option<String>,
    pub decision_payload_digest: Option<String>,
    pub decision_result_json: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub activated_snapshot_id: Option<String>,
    pub host_deployment_id: Option<String>,
    pub activation_error: Option<String>,
}
