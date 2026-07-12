//! Durable gate evidence registration (R3A-R2).
//! Accepts only `gate_attempt_id`. Uses unified validator for source chain.

use crate::hcr::validate;
use crate::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;

/// Register gate evidence by validating the full source chain.
/// Only accepts `gate_attempt_id` — no caller-supplied results.
pub fn register_gate_evidence(journal: &JournalStore, gate_attempt_id: &str) -> Result<String> {
    let parsed = validate::validate_gate_source_chain(journal, gate_attempt_id)?;
    let evidence_id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    let now = Utc::now().to_rfc3339();
    journal.insert_evidence_atomically(
        &evidence_id,
        &parsed.gate_attempt_id,
        &parsed.receipt_event_id,
        &parsed.receipt_payload_digest,
        &now,
    )?;
    Ok(evidence_id)
}
