//! Atomic HCR settlement — all validation inside BEGIN IMMEDIATE (R3A-R4).
//! Sole entry point `settle_hcr()` accepts only identity keys.

use crate::domain::SettlementResult;
use crate::journal::JournalStore;
use anyhow::Result;

pub fn settle_hcr(
    journal: &JournalStore,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
) -> Result<SettlementResult> {
    journal.settle_hcr_in_tx(hcr_id, claim_id, run_id)
}
