//! Unified source-chain validator for HCR gate evidence (R3A-R2).
//!
//! Used by: register_gate_evidence, settlement, resume.
//! Loads and validates the full chain: HCR → claim → Run → RunMode →
//! Gate Attempt → InvocationProposed → ReceiptReceived.
//! Returns a `ValidatedGateReceipt` with values parsed from the receipt event.

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

/// Validate a gate's source chain and return the parsed receipt.
/// All validation happens in one call — no separate "outcome" from cache.
pub fn validate_gate_source_chain(
    journal: &JournalStore,
    gate_attempt_id: &str,
) -> Result<ValidatedGateReceipt> {
    let attempt = journal
        .get_gate_attempt(gate_attempt_id)?
        .ok_or_else(|| anyhow::anyhow!("VALIDATE_ATTEMPT_NOT_FOUND: {gate_attempt_id}"))?;

    let hcr_id = &attempt.hcr_id;
    let claim_id = &attempt.claim_id;
    let run_id = &attempt.run_id;
    let intent_id = &attempt.invocation_intent_id;

    // 1. HCR must exist and be running.
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("VALIDATE_HCR_NOT_FOUND: {hcr_id}"))?;
    if hcr.status != "running" {
        bail!(
            "VALIDATE_HCR_NOT_RUNNING: HCR {hcr_id} status {}",
            hcr.status
        );
    }

    // 2. Active claim must match.
    let claim = journal
        .get_active_claim_for_hcr(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("VALIDATE_NO_CLAIM: {hcr_id}"))?;
    if claim.claim_id.0 != *claim_id {
        bail!("VALIDATE_CLAIM_MISMATCH");
    }

    // 3. Run binding must match.
    let binding = journal
        .get_run_binding_for_claim(claim_id)?
        .ok_or_else(|| anyhow::anyhow!("VALIDATE_NO_BINDING: {claim_id}"))?;
    if binding.run_id != *run_id {
        bail!("VALIDATE_RUN_MISMATCH");
    }
    if binding.hcr_id != *hcr_id {
        bail!("VALIDATE_BINDING_HCR_MISMATCH");
    }

    // 4. Persisted Run must be in RunMode::Hcr with matching fields.
    let run = journal
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("VALIDATE_RUN_NOT_FOUND: {run_id}"))?;
    match &run.mode {
        RunMode::Hcr {
            hcr_id: rh,
            claim_id: rc,
            harness_id: rhh,
        } => {
            if rh != hcr_id {
                bail!("VALIDATE_RUNMODE_HCR_MISMATCH");
            }
            if rc != claim_id {
                bail!("VALIDATE_RUNMODE_CLAIM_MISMATCH");
            }
            if rhh != &attempt.harness_id {
                bail!("VALIDATE_RUNMODE_HARNESS_MISMATCH");
            }
        }
        _ => bail!("VALIDATE_RUN_NOT_HCR"),
    }

    // 5. InvocationProposed event must exist with correct operation.
    let events = journal.events()?;
    let intent_ev = events
        .iter()
        .find(|e| {
            e.kind == JournalEventKind::InvocationProposed
                && e.correlation_id.as_deref() == Some(intent_id)
        })
        .ok_or_else(|| anyhow::anyhow!("VALIDATE_INTENT_NOT_FOUND: {intent_id}"))?;

    if intent_ev.run_id.as_ref().map(|r| r.0.as_str()) != Some(run_id) {
        bail!("VALIDATE_INTENT_RUN_MISMATCH");
    }
    let intent_op = intent_ev
        .payload
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if intent_op != attempt.expected_operation {
        bail!(
            "VALIDATE_OP_MISMATCH: expected {}, got {}",
            attempt.expected_operation,
            intent_op
        );
    }

    // 6. Exactly one ReceiptReceived event must exist for this intent.
    let receipts: Vec<&JournalEvent> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.correlation_id.as_deref() == Some(intent_id)
        })
        .collect();

    if receipts.is_empty() {
        bail!("VALIDATE_NO_RECEIPT: no receipt for intent {intent_id}");
    }
    if receipts.len() > 1 {
        bail!(
            "VALIDATE_CONFLICTING_RECEIPTS: {} for intent {}",
            receipts.len(),
            intent_id
        );
    }

    let receipt_ev = receipts[0];
    if receipt_ev.run_id.as_ref().map(|r| r.0.as_str()) != Some(run_id) {
        bail!("VALIDATE_RECEIPT_RUN_MISMATCH");
    }

    // 7. Parse receipt fields from the journal event payload.
    let out = receipt_ev.payload.get("output");
    let exit_code = out
        .and_then(|o| o.get("exit_code"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1) as i32;
    let timed_out = out
        .and_then(|o| o.get("timed_out"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let child_cleanup = out
        .and_then(|o| o.get("child_cleanup"))
        .and_then(|v| v.as_bool());
    let err_cat = out
        .and_then(|o| o.get("error_category"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let status = receipt_ev
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    // 8. Compute digest of the receipt payload.
    let mut hasher = Sha256::new();
    hasher.update(format!("{:?}", receipt_ev.payload).as_bytes());
    let receipt_payload_digest = format!("sha256:{}", hex::encode(hasher.finalize()));

    Ok(ValidatedGateReceipt {
        gate_attempt_id: gate_attempt_id.to_string(),
        receipt_event_id: receipt_ev.event_id.0.clone(),
        status,
        exit_code,
        timed_out,
        child_cleanup,
        error_code: err_cat,
        receipt_payload_digest,
        operation: intent_op.to_string(),
    })
}
