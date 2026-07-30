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

/// Generic binding between an externally issued acceptance receipt and the
/// existing CapabilityChangeProposal governance primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityProposalReceiptLink {
    pub proposal_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub acceptance_invocation_id: String,
    pub issuer_principal_id: String,
    pub operation: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub artifact_ref: String,
    pub artifact_digest: String,
    pub manifest_ref: String,
    pub manifest_digest: String,
    pub evidence_digest: String,
    pub receipt_digest: String,
    pub acceptance_outcome: String,
    pub contract_catalog_version: String,
    pub profile_id: String,
    pub profile_catalog_version: String,
    pub source_registry_snapshot_id: String,
    pub origin_run_id: String,
    pub origin_session_id: String,
    pub created_at: String,
}
