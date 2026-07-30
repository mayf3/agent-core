//! Read-only domain shape for retained historical HCR settlements.
//!
//! Active HCR workflow types intentionally do not live in the Kernel after
//! retirement. Existing tables and Journal events remain immutable and
//! queryable through the legacy read surface.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcrSettlement {
    pub settlement_id: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub result: String,
    pub error_code: Option<String>,
    pub evidence_set_digest: String,
    pub failure_evidence_event_id: Option<String>,
    pub created_at: String,
}
