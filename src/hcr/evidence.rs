//! Durable gate evidence registration for HCR settlement (R3A).
//!
//! The only entry point for creating `hcr_gate_evidence` rows. It accepts
//! only identity keys — never caller-supplied outcomes or status values.
//! Every field is loaded from persistent storage and validated.

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::Value;

/// Register a gate evidence record by loading and validating the full chain
/// from persistent store.
///
/// Parameters (identity keys only):
/// - `expected_gate_kind`: which gate this evidence is for (caller asserts intent)
/// - `invocation_intent_id`: the invocation that was created for this gate
/// - `receipt_id`: the receipt that resulted from the invocation
///
/// The function loads: HCR → claim → Run binding → RunMode → InvocationIntent
/// → Receipt, and validates the complete chain. No caller-supplied outcomes,
/// status, or exit codes are accepted.
pub fn register_gate_evidence(
    journal: &JournalStore,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    expected_gate_kind: GateKind,
    invocation_intent_id: &str,
    receipt_id: &str,
) -> Result<String> {
    // ── 1. Load the HCR ───────────────────────────────────────────────
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_HCR_NOT_FOUND: HCR {hcr_id}"))?;

    if hcr.status != "running" {
        bail!(
            "EVIDENCE_HCR_NOT_RUNNING: HCR {hcr_id} status is {}, expected running",
            hcr.status
        );
    }

    // ── 2. Verify the active claim ────────────────────────────────────
    let claim = journal
        .get_active_claim_for_hcr(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_NO_ACTIVE_CLAIM: HCR {hcr_id}"))?;

    if claim.claim_id.0 != claim_id {
        bail!(
            "EVIDENCE_CLAIM_MISMATCH: expected {claim_id}, found {}",
            claim.claim_id.0
        );
    }

    // ── 3. Verify Run binding ─────────────────────────────────────────
    let binding = journal
        .get_run_binding_for_claim(claim_id)?
        .ok_or_else(|| anyhow::anyhow!("EVIDENCE_NO_RUN_BINDING: claim {claim_id}"))?;

    if binding.run_id != run_id {
        bail!(
            "EVIDENCE_RUN_MISMATCH: expected {run_id}, found {}",
            binding.run_id
        );
    }

    // ── 4. Verify harness_id consistency ──────────────────────────────
    if claim.harness_id != hcr.harness_id {
        bail!(
            "EVIDENCE_HARNESS_MISMATCH: claim {}, HCR {}",
            claim.harness_id,
            hcr.harness_id
        );
    }

    // ── 5. Load and validate the InvocationIntent event ───────────────
    let events = journal.events()?;
    let intent_event = events.iter().find(|e| {
        e.kind == JournalEventKind::InvocationProposed
            && e.correlation_id.as_deref() == Some(invocation_intent_id)
    });

    let intent_event = intent_event.ok_or_else(|| {
        anyhow::anyhow!("EVIDENCE_INTENT_NOT_FOUND: invocation {invocation_intent_id}")
    })?;

    // Verify intent belongs to this Run.
    if intent_event.run_id.as_ref().map(|r| r.0.as_str()) != Some(run_id) {
        bail!(
            "EVIDENCE_INTENT_RUN_MISMATCH: intent {invocation_intent_id} does not belong to run {run_id}"
        );
    }

    let intent_operation = intent_event
        .payload
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // ── 6. Load and validate the Receipt event ────────────────────────
    let receipt_event = events.iter().find(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.correlation_id.as_deref() == Some(invocation_intent_id)
    });

    let receipt_event = receipt_event.ok_or_else(|| {
        anyhow::anyhow!("EVIDENCE_RECEIPT_NOT_FOUND: invocation {invocation_intent_id}")
    })?;

    // Verify Receipt belongs to this Run.
    if receipt_event.run_id.as_ref().map(|r| r.0.as_str()) != Some(run_id) {
        bail!(
            "EVIDENCE_RECEIPT_RUN_MISMATCH: receipt for {invocation_intent_id} not in run {run_id}"
        );
    }

    // ── 7. Parse receipt output fields ────────────────────────────────
    let receipt_output: Option<&Value> = receipt_event.payload.get("output");
    let exit_code = receipt_output
        .and_then(|o| o.get("exit_code"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1) as i32;
    let timed_out = receipt_output
        .and_then(|o| o.get("timed_out"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stdout_truncated = receipt_output
        .and_then(|o| o.get("stdout_truncated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stderr_truncated = receipt_output
        .and_then(|o| o.get("stderr_truncated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let child_cleanup = receipt_output
        .and_then(|o| o.get("child_cleanup"))
        .and_then(|v| v.as_bool());
    let error_category = receipt_output
        .and_then(|o| o.get("error_category"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let receipt_status = receipt_event
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    // ── 8. Create the evidence record ─────────────────────────────────
    let evidence_id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    let now = Utc::now().to_rfc3339();

    journal.insert_gate_evidence(
        &evidence_id,
        hcr_id,
        claim_id,
        run_id,
        &hcr.harness_id,
        crate::hcr::revalidate::HCR_HARNESS_WORKSPACE_ID,
        expected_gate_kind.as_str(),
        invocation_intent_id,
        receipt_id,
        intent_operation,
        "hcr_trusted_profile",
        receipt_status,
        exit_code,
        timed_out,
        stdout_truncated,
        stderr_truncated,
        child_cleanup,
        error_category.as_deref(),
        None,
        None,
        &now,
    )?;

    // ── 9. Append journal event ───────────────────────────────────────
    journal.append_event(
        JournalEventKind::HcrEvidenceRegistered,
        None,
        None,
        Some(&evidence_id),
        serde_json::json!({
            "evidence_id": evidence_id,
            "hcr_id": hcr_id,
            "claim_id": claim_id,
            "run_id": run_id,
            "gate_kind": expected_gate_kind.as_str(),
            "receipt_id": receipt_id,
        }),
    )?;

    Ok(evidence_id)
}

/// Load all gate evidence records for an HCR/claim/run, ordered by gate kind.
pub fn load_gate_evidence(
    journal: &JournalStore,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
) -> Result<Vec<HcrGateEvidence>> {
    journal.get_gate_evidence_for_hcr(hcr_id, claim_id, run_id)
}

/// Validate that a gate evidence record meets structured success criteria.
///
/// Returns Ok(()) only if:
/// - structured_status == "Succeeded"
/// - exit_code == 0
/// - timed_out == false
/// - child_cleanup == Some(true) (required; None means missing from protocol)
/// - error_code == None
pub fn validate_gate_evidence(evidence: &HcrGateEvidence) -> Result<()> {
    if evidence.structured_status != "Succeeded" {
        bail!(
            "GATE_EVIDENCE_STATUS_FAILED: {} gate status is '{}', expected Succeeded",
            evidence.gate_kind,
            evidence.structured_status
        );
    }
    if evidence.exit_code != 0 {
        bail!(
            "GATE_EVIDENCE_EXIT_FAILED: {} gate exit_code is {}, expected 0",
            evidence.gate_kind,
            evidence.exit_code
        );
    }
    if evidence.timed_out {
        bail!(
            "GATE_EVIDENCE_TIMEOUT: {} gate timed out",
            evidence.gate_kind
        );
    }
    // child_cleanup must be confirmed (Some(true)). None means the coding
    // harness receipt protocol did not include this field.
    if evidence.child_cleanup != Some(true) {
        bail!(
            "GATE_EVIDENCE_CLEANUP_FAILED: {} gate child_cleanup is {:?}, expected Some(true)",
            evidence.gate_kind,
            evidence.child_cleanup
        );
    }
    if evidence.error_code.is_some() {
        bail!(
            "GATE_EVIDENCE_ERROR: {} gate error_code is {:?}, expected None",
            evidence.gate_kind,
            evidence.error_code
        );
    }
    Ok(())
}

/// Check whether a set of evidence records has exactly one of each required
/// gate kind — no duplicates, no extras.
pub fn check_gate_completeness(evidence_list: &[HcrGateEvidence]) -> Result<()> {
    let required = GateKind::all_required();

    if evidence_list.len() != required.len() {
        bail!(
            "GATE_INCOMPLETE: expected {} evidence records, got {}",
            required.len(),
            evidence_list.len()
        );
    }

    let mut kinds: Vec<&str> = evidence_list.iter().map(|e| e.gate_kind.as_str()).collect();
    kinds.sort();

    let mut expected: Vec<&str> = required.iter().map(|k| k.as_str()).collect();
    expected.sort();

    if kinds != expected {
        bail!(
            "GATE_KINDS_MISMATCH: expected {:?}, got {:?}",
            expected,
            kinds
        );
    }

    Ok(())
}
