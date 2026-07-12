//! Atomic HCR settlement from persisted evidence only (R3A-R1).
//!
//! The sole entry point `settle_hcr()` accepts only `(hcr_id, claim_id, run_id)`.
//! Everything is loaded and re-verified inside a single SQLite transaction.
//! No caller-supplied result, status, or outcome is accepted.
//!
//! Within the transaction:
//! 1. Reload HCR, claim, Run, RunMode, bindings
//! 2. Load five GateAttempts + five Evidence + five Intent/Receipt events
//! 3. Re-validate full chain and structured success
//! 4. Compute result and evidence_set_digest
//! 5. CAS update HCR status
//! 6. Insert settlement record
//! 7. Insert terminal Journal event
//! 8. COMMIT (any failure rolls back)

use crate::domain::*;
use crate::hcr::evidence;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

/// Settle an HCR by loading all facts from store and computing the result
/// atomically. No caller-supplied outcomes accepted.
pub fn settle_hcr(
    journal: &JournalStore,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
) -> Result<SettlementResult> {
    // ── 1. Reload HCR and verify state ────────────────────────────────
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_HCR_NOT_FOUND: {hcr_id}"))?;

    // Check terminal state early.
    if hcr.status == "succeeded" || hcr.status == "failed" {
        return if let Some(stl) = journal.get_settlement(hcr_id)? {
            Ok(settlement_to_result(&stl))
        } else {
            Ok(SettlementResult::EvidenceIncomplete(
                "terminal HCR, no settlement record".into(),
            ))
        };
    }
    if hcr.status != "running" {
        bail!("SETTLE_HCR_NOT_RUNNING: HCR {hcr_id} status {}", hcr.status);
    }

    // ── 2. Verify claim ───────────────────────────────────────────────
    let claim = journal
        .get_active_claim_for_hcr(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_NO_CLAIM: {hcr_id}"))?;
    if claim.claim_id.0 != claim_id {
        bail!("SETTLE_CLAIM_MISMATCH");
    }

    // ── 3. Verify Run binding ─────────────────────────────────────────
    let binding = journal
        .get_run_binding_for_claim(claim_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_NO_BINDING: {claim_id}"))?;
    if binding.run_id != *run_id {
        bail!("SETTLE_RUN_MISMATCH");
    }

    // ── 4. Load Run and verify RunMode ────────────────────────────────
    let run = journal
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_RUN_NOT_FOUND: {run_id}"))?;
    match &run.mode {
        RunMode::Hcr {
            hcr_id: rh,
            claim_id: rc,
            harness_id: _,
        } => {
            if rh != hcr_id {
                bail!("SETTLE_RUNMODE_HCR_MISMATCH");
            }
            if rc != claim_id {
                bail!("SETTLE_RUNMODE_CLAIM_MISMATCH");
            }
        }
        _ => bail!("SETTLE_RUN_NOT_HCR"),
    }

    // ── 5. Load all events for receipt verification ───────────────────
    let events = journal.events()?;

    // ── 6. Load five GateAttempts and verify completeness ─────────────
    let attempts = journal.get_attempts_for_hcr(hcr_id, claim_id, run_id)?;
    if attempts.len() != 5 {
        return Ok(SettlementResult::EvidenceIncomplete(format!(
            "expected 5 gate attempts, got {}",
            attempts.len()
        )));
    }

    let attempt_kinds: Vec<&str> = attempts.iter().map(|a| a.gate_kind.as_str()).collect();
    let required: Vec<&str> = GateKind::all_required()
        .iter()
        .map(|k| k.as_str())
        .collect();
    if attempt_kinds != required {
        return Ok(SettlementResult::EvidenceIncomplete(format!(
            "expected gate kinds {:?}, got {:?}",
            required, attempt_kinds
        )));
    }

    // ── 7. Load five Evidence records ─────────────────────────────────
    let evidence_list = journal.get_evidence_for_attempts(
        &attempts
            .iter()
            .map(|a| a.gate_attempt_id.as_str())
            .collect::<Vec<_>>(),
    )?;

    if evidence_list.len() != 5 {
        return Ok(SettlementResult::EvidenceIncomplete(format!(
            "expected 5 evidence, got {}",
            evidence_list.len()
        )));
    }

    // ── 8. For each evidence, re-verify against receipt event ─────────
    let mut infra_failure = false;
    let mut candidate_failure = false;
    let mut first_error = String::new();

    for ev in &evidence_list {
        // Find the attempt.
        let attempt = match attempts
            .iter()
            .find(|a| a.gate_attempt_id == ev.gate_attempt_id)
        {
            Some(a) => a,
            None => {
                return Ok(SettlementResult::EvidenceConflict(format!(
                    "evidence {} has no matching attempt",
                    ev.evidence_id
                )));
            }
        };

        // Find the receipt event.
        let receipt_ev = match events.iter().find(|e| e.event_id.0 == ev.receipt_event_id) {
            Some(e) => e,
            None => {
                infra_failure = true;
                first_error = format!(
                    "receipt event {} not found for evidence {}",
                    ev.receipt_event_id, ev.evidence_id
                );
                continue;
            }
        };

        // Verify receipt is a ReceiptReceived.
        if receipt_ev.kind != JournalEventKind::ReceiptReceived {
            infra_failure = true;
            first_error = format!("event {} is not ReceiptReceived", ev.receipt_event_id);
            continue;
        }

        // Verify payload digest matches.
        if let Err(e) = evidence::verify_evidence_against_receipt(ev, receipt_ev) {
            infra_failure = true;
            first_error = format!("evidence tamper detected: {e}");
            continue;
        }

        // Verify operation matches attempt.
        let receipt_op = receipt_ev
            .payload
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if receipt_op != attempt.expected_operation && !receipt_op.is_empty() {
            candidate_failure = true;
            first_error = format!(
                "operation mismatch for {}: expected {}, receipt has {}",
                attempt.gate_kind, attempt.expected_operation, receipt_op
            );
            continue;
        }

        // ── Structured success check ──────────────────────────────────
        if ev.structured_status != "Succeeded" {
            // Check if it's an infrastructure error or candidate failure.
            let has_infra = ev.error_code.as_deref().map_or(false, |c| {
                matches!(
                    c,
                    "harness_failed"
                        | "timeout"
                        | "connect_failed"
                        | "adapter_timeout"
                        | "protocol_mismatch"
                        | "malformed_response"
                )
            });
            if ev.timed_out
                || ev.child_cleanup == Some(false)
                || ev.child_cleanup.is_none()
                || has_infra
            {
                infra_failure = true;
            } else {
                candidate_failure = true;
            }
            first_error = format!(
                "{} gate failed: status={}, exit={}, timed_out={}, cleanup={:?}, error={:?}",
                attempt.gate_kind,
                ev.structured_status,
                ev.exit_code,
                ev.timed_out,
                ev.child_cleanup,
                ev.error_code
            );
            continue;
        }
        if ev.exit_code != 0 {
            candidate_failure = true;
            first_error = format!("{} gate exit code {}", attempt.gate_kind, ev.exit_code);
            continue;
        }
        if ev.timed_out {
            infra_failure = true;
            first_error = format!("{} gate timed out", attempt.gate_kind);
            continue;
        }
        if ev.child_cleanup != Some(true) {
            infra_failure = true;
            first_error = format!(
                "{} gate child_cleanup is {:?}",
                attempt.gate_kind, ev.child_cleanup
            );
            continue;
        }
        if ev.error_code.is_some() {
            infra_failure = true;
            first_error = format!(
                "{} gate error_code is {:?}",
                attempt.gate_kind, ev.error_code
            );
            continue;
        }
    }

    // ── 9. Determine result ───────────────────────────────────────────
    if infra_failure && !candidate_failure {
        return Ok(SettlementResult::InfrastructureFailure(first_error));
    }
    if infra_failure && candidate_failure {
        return Ok(SettlementResult::InfrastructureFailure(format!(
            "infrastructure failure with candidate issues: {first_error}"
        )));
    }
    if candidate_failure {
        // Write terminal failed via the atomic store method (internal only).
        let digest =
            compute_evidence_digest(hcr_id, claim_id, run_id, &attempts, &evidence_list, &events);
        let settlement_id = journal.settle_hcr_terminal(
            hcr_id,
            claim_id,
            run_id,
            "candidate_failed",
            Some(&first_error),
            &digest,
        )?;
        return Ok(SettlementResult::CandidateFailed(settlement_id));
    }

    // ── 10. All gates passed — successful settlement ──────────────────
    let digest =
        compute_evidence_digest(hcr_id, claim_id, run_id, &attempts, &evidence_list, &events);
    let settlement_id =
        journal.settle_hcr_terminal(hcr_id, claim_id, run_id, "succeeded", None, &digest)?;
    Ok(SettlementResult::Succeeded(settlement_id))
}

/// Compute a canonical evidence_set_digest covering all relevant fields.
fn compute_evidence_digest(
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    attempts: &[HcrGateAttempt],
    evidence_list: &[HcrGateEvidence],
    events: &[JournalEvent],
) -> String {
    use sha2::Digest;
    let mut hasher = Sha256::new();
    hasher.update(hcr_id.as_bytes());
    hasher.update(b"|");
    hasher.update(claim_id.as_bytes());
    hasher.update(b"|");
    hasher.update(run_id.as_bytes());
    hasher.update(b"|");

    for kind in GateKind::all_required() {
        let kind_str = kind.as_str();
        let attempt = attempts.iter().find(|a| a.gate_kind == kind_str);
        let evidence = evidence_list
            .iter()
            .find(|e| attempt.map_or(false, |a| a.gate_attempt_id == e.gate_attempt_id));

        hasher.update(kind_str.as_bytes());
        hasher.update(b"|");
        if let Some(a) = attempt {
            hasher.update(a.gate_attempt_id.as_bytes());
            hasher.update(b"|");
            hasher.update(a.invocation_intent_id.as_bytes());
            hasher.update(b"|");
        }
        if let Some(ev) = evidence {
            hasher.update(ev.receipt_event_id.as_bytes());
            hasher.update(b"|");
            hasher.update(ev.structured_status.as_bytes());
            hasher.update(b"|");
            hasher.update(&ev.exit_code.to_le_bytes());
            hasher.update(b"|");
            hasher.update(ev.timed_out.to_string().as_bytes());
            hasher.update(b"|");
            hasher.update(
                ev.child_cleanup
                    .map(|c| c.to_string())
                    .unwrap_or_default()
                    .as_bytes(),
            );
            hasher.update(b"|");
            hasher.update(ev.error_code.as_deref().unwrap_or("").as_bytes());
            hasher.update(b"|");
            hasher.update(ev.receipt_payload_digest.as_bytes());
            hasher.update(b"|");
        }
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn settlement_to_result(stl: &HcrSettlement) -> SettlementResult {
    match stl.result.as_str() {
        "succeeded" => SettlementResult::Succeeded(stl.settlement_id.clone()),
        "candidate_failed" => SettlementResult::CandidateFailed(stl.settlement_id.clone()),
        _ => SettlementResult::InfrastructureFailure(format!(
            "unknown settlement result: {}",
            stl.result
        )),
    }
}
