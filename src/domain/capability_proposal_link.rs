//! Trusted binding between a CapabilityChangeProposal and its originating HCR
//! settlement. Created atomically with the proposal in a single transaction.
//!
//! Every field is NOT NULL and immutable after creation. The UNIQUE constraint
//! on (hcr_id, candidate_digest, operation) prevents duplicate proposals for
//! the same developed capability.

use serde::{Deserialize, Serialize};

/// Trusted link between a CapabilityChangeProposal and its originating HCR
/// settlement evidence. Created atomically with the proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityProposalHcrLink {
    pub proposal_id: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub operation: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub artifact_ref: String,
    pub artifact_digest: String,
    pub evidence_digest: String,
    pub source_registry_snapshot_id: String,
    pub settlement_id: String,
    pub created_at: String,
}
