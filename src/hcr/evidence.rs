//! Durable gate evidence registration (R3A-R1).
//!
//! The only entry point `register_gate_evidence()` accepts only a
//! `gate_attempt_id`. Loads everything from persistent store:
//! attempt → invocation intent → receipt journal event.
//! No caller-supplied outcomes, receipt_ids, or statuses.

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};

/// Register gate evidence by loading the canonical gate attempt and
/// finding the receipt from the journal. Accepts only `gate_attempt_id`.
pub fn register_gate_evidence(journal: &JournalStore, gate_attempt_id: &str) -> Result<String> {
    // ── 1. Load gate attempt ──────────────────────────────────────────
    let attempt = journal
        .get_gate_attempt(gate_attempt_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_ATTEMPT_NOT_FOUND: {gate_attempt_id}"))?;

    let hcr_id = &attempt.hcr_id;
    let claim_id = &attempt.claim_id;
    let run_id = &attempt.run_id;
    let intent_id = &attempt.invocation_intent_id;

    // ── 2. Reload HCR ─────────────────────────────────────────────────
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_HCR_NOT_FOUND: {hcr_id}"))?;
    if hcr.status != "running" {
        bail!(
            "EVIDENCE_HCR_NOT_RUNNING: HCR {hcr_id} status {}",
            hcr.status
        );
    }

    // ── 3. Verify active claim ────────────────────────────────────────
    let claim = journal
        .get_active_claim_for_hcr(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_NO_CLAIM: {hcr_id}"))?;
    if claim.claim_id.0 != *claim_id {
        bail!("EVIDENCE_CLAIM_MISMATCH");
    }

    // ── 4. Verify Run binding ─────────────────────────────────────────
    let binding = journal
        .get_run_binding_for_claim(claim_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_NO_BINDING: {claim_id}"))?;
    if binding.run_id != *run_id {
        bail!("EVIDENCE_RUN_MISMATCH");
    }

    // ── 5. Load persisted Run, verify RunMode::Hcr ────────────────────
    let run = journal
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_RUN_NOT_FOUND: {run_id}"))?;
    match &run.mode {
        RunMode::Hcr {
            hcr_id: rh,
            claim_id: rc,
            harness_id: rhh,
        } => {
            if rh != hcr_id {
                bail!("EVIDENCE_RUNMODE_HCR_MISMATCH");
            }
            if rc != claim_id {
                bail!("EVIDENCE_RUNMODE_CLAIM_MISMATCH");
            }
            if rhh != &attempt.harness_id {
                bail!("EVIDENCE_RUNMODE_HARNESS_MISMATCH");
            }
        }
        _ => bail!("EVIDENCE_RUN_NOT_HCR"),
    }

    // ── 6. Load InvocationProposed event ──────────────────────────────
    let events = journal.events()?;
    let intent_ev = events
        .iter()
        .find(|e| {
            e.kind == JournalEventKind::InvocationProposed
                && e.correlation_id.as_deref() == Some(intent_id)
        })
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_INTENT_NOT_FOUND: {intent_id}"))?;
    if intent_ev.run_id.as_ref().map(|r| r.0.as_str()) != Some(run_id) {
        bail!("EVIDENCE_INTENT_RUN_MISMATCH");
    }
    let intent_op = intent_ev
        .payload
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if intent_op != attempt.expected_operation {
        bail!(
            "EVIDENCE_OP_MISMATCH: expected {}, intent has {}",
            attempt.expected_operation,
            intent_op
        );
    }

    // ── 7. Find unique ReceiptReceived event ──────────────────────────
    let receipts: Vec<&JournalEvent> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.correlation_id.as_deref() == Some(intent_id)
        })
        .collect();

    if receipts.is_empty() {
        return Ok(format!("EVIDENCE_NO_RECEIPT_YET:{}", intent_id));
    }
    if receipts.len() > 1 {
        bail!(
            "EVIDENCE_CONFLICTING_RECEIPTS: {} for intent {}",
            receipts.len(),
            intent_id
        );
    }

    let receipt_ev = receipts[0];
    if receipt_ev.run_id.as_ref().map(|r| r.0.as_str()) != Some(run_id) {
        bail!("EVIDENCE_RECEIPT_RUN_MISMATCH");
    }

    // ── 8. Parse receipt fields ──────────────────────────────────────
    let out = receipt_ev.payload.get("output");
    let exit_code = out
        .and_then(|o| o.get("exit_code"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1) as i32;
    let timed_out = out
        .and_then(|o| o.get("timed_out"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stdout_trunc = out
        .and_then(|o| o.get("stdout_truncated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stderr_trunc = out
        .and_then(|o| o.get("stderr_truncated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let child_cleanup = out
        .and_then(|o| o.get("child_cleanup"))
        .and_then(|v| v.as_bool());
    let err_cat = out
        .and_then(|o| o.get("error_category"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let receipt_status = receipt_ev
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    // ── 9. Payload digest ─────────────────────────────────────────────
    let payload_digest = compute_payload_digest(&receipt_ev.payload);

    // ── 10. Insert evidence (atomic with journal) ─────────────────────
    let evidence_id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    let now = Utc::now().to_rfc3339();

    journal.insert_evidence_atomically(
        &evidence_id,
        gate_attempt_id,
        &receipt_ev.event_id.0,
        receipt_status,
        exit_code,
        timed_out,
        stdout_trunc,
        stderr_trunc,
        child_cleanup,
        err_cat.as_deref(),
        &payload_digest,
        &now,
    )?;

    Ok(evidence_id)
}

fn compute_payload_digest(payload: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    if let Some(s) = payload.get("status").and_then(|v| v.as_str()) {
        hasher.update(s.as_bytes());
    }
    if let Some(output) = payload.get("output") {
        hasher.update(serde_json::to_string(output).unwrap_or_default().as_bytes());
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Verify evidence cached fields against the original receipt event.
pub fn verify_evidence_against_receipt(
    evidence: &HcrGateEvidence,
    receipt: &JournalEvent,
) -> Result<()> {
    if evidence.receipt_event_id != receipt.event_id.0 {
        bail!("EVIDENCE_RECEIPT_EVENT_ID_MISMATCH");
    }
    let expected = compute_payload_digest(&receipt.payload);
    if evidence.receipt_payload_digest != expected {
        bail!("EVIDENCE_PAYLOAD_DIGEST_MISMATCH");
    }
    Ok(())
}
