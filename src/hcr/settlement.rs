//! Atomic HCR settlement from persisted gate evidence only (R3A).
//!
//! The settlement entry point `settle_hcr()` accepts only identity keys
//! (hcr_id, claim_id, run_id) and loads everything from the database.
//! No caller-supplied outcomes, statuses, or Vec<GateOutcome> are accepted.
//!
//! The state update and terminal journal event are written in a single
//! SQLite transaction via `JournalStore::settle_hcr_atomically()`.

use crate::domain::*;
use crate::hcr::evidence;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

/// Settlement result classification returned by `settle_hcr()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettleOutcome {
    /// All gates passed structured validation; HCR transitioned to terminal
    /// succeeded.
    Succeeded(String), // settlement_id
    /// One or more gates failed definitively; HCR transitioned to terminal
    /// failed.
    CandidateFailed {
        settlement_id: String,
        error_code: String,
    },
    /// Infrastructure or protocol error prevented settlement; HCR stays
    /// running for retry.
    RetryableInfrastructureFailure(String),
    /// HCR was already settled by another caller.
    AlreadySettled {
        settlement_id: String,
        result: String,
    },
    /// Evidence set is incomplete or has conflicts.
    EvidenceIncomplete(String),
    EvidenceConflict(String),
}

/// Settle an HCR by loading all five required gate evidence records from
/// the database, validating each against structured success criteria,
/// and atomically transitioning the HCR to terminal state.
///
/// This is the ONLY settlement entry point. It accepts only identity keys.
pub fn settle_hcr(
    journal: &JournalStore,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
) -> Result<SettleOutcome> {
    // ── 1. Reload HCR and verify state ────────────────────────────────
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_HCR_NOT_FOUND: {hcr_id}"))?;

    if hcr.status == "succeeded" || hcr.status == "failed" {
        // Already terminal — check settlement record.
        if let Some(settlement) = journal.get_hcr_settlement(hcr_id)? {
            return Ok(SettleOutcome::AlreadySettled {
                settlement_id: settlement.settlement_id,
                result: settlement.result,
            });
        }
        return Ok(SettleOutcome::AlreadySettled {
            settlement_id: String::new(),
            result: hcr.status.clone(),
        });
    }

    if hcr.status != "running" {
        bail!(
            "SETTLE_HCR_NOT_RUNNING: HCR {hcr_id} status is {}, expected running",
            hcr.status
        );
    }

    // ── 2. Verify claim and Run binding ───────────────────────────────
    let claim = journal
        .get_active_claim_for_hcr(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_NO_ACTIVE_CLAIM: HCR {hcr_id}"))?;

    if claim.claim_id.0 != claim_id {
        bail!(
            "SETTLE_CLAIM_MISMATCH: expected {claim_id}, found {}",
            claim.claim_id.0
        );
    }

    let binding = journal
        .get_run_binding_for_claim(claim_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_NO_RUN_BINDING: claim {claim_id}"))?;

    if binding.run_id != run_id {
        bail!(
            "SETTLE_RUN_MISMATCH: expected {run_id}, found {}",
            binding.run_id
        );
    }

    // ── 3. Load gate evidence from store only ─────────────────────────
    let evidence_list = evidence::load_gate_evidence(journal, hcr_id, claim_id, run_id)?;

    // ── 4. Check completeness (exactly 5 distinct gates) ──────────────
    if let Err(e) = evidence::check_gate_completeness(&evidence_list) {
        return Ok(SettleOutcome::EvidenceIncomplete(e.to_string()));
    }

    // ── 5. Validate each gate's evidence ──────────────────────────────
    let mut all_passed = true;
    let mut first_failure = String::new();

    for ev in &evidence_list {
        if let Err(e) = evidence::validate_gate_evidence(ev) {
            all_passed = false;
            first_failure = e.to_string();
            break;
        }
    }

    // ── 6. Compute evidence set digest for idempotency ────────────────
    let digest = compute_evidence_digest(&evidence_list);

    // ── 7. Atomically settle ─────────────────────────────────────────
    if all_passed {
        match journal.settle_hcr_atomically(hcr_id, claim_id, run_id, "succeeded", None, &digest) {
            Ok(settlement_id) => Ok(SettleOutcome::Succeeded(settlement_id)),
            Err(e) => {
                // CAS failed — another worker settled first.
                if let Some(settlement) = journal.get_hcr_settlement(hcr_id)? {
                    Ok(SettleOutcome::AlreadySettled {
                        settlement_id: settlement.settlement_id,
                        result: settlement.result,
                    })
                } else {
                    Err(e)
                }
            }
        }
    } else {
        // Gate failure is candidate_failed, not infrastructure.
        let error_code = if first_failure.contains("EVIDENCE_")
            || first_failure.contains("CLEANUP")
            || first_failure.contains("TIMEOUT")
            || first_failure.contains("ERROR")
        {
            "gate_evidence_failed"
        } else {
            "gate_failed"
        };

        match journal.settle_hcr_atomically(
            hcr_id,
            claim_id,
            run_id,
            "failed",
            Some(error_code),
            &digest,
        ) {
            Ok(settlement_id) => Ok(SettleOutcome::CandidateFailed {
                settlement_id,
                error_code: error_code.to_string(),
            }),
            Err(e) => {
                if let Some(settlement) = journal.get_hcr_settlement(hcr_id)? {
                    Ok(SettleOutcome::AlreadySettled {
                        settlement_id: settlement.settlement_id,
                        result: settlement.result,
                    })
                } else {
                    Err(e)
                }
            }
        }
    }
}

/// Compute a stable digest of the evidence set for idempotency.
fn compute_evidence_digest(evidence_list: &[HcrGateEvidence]) -> String {
    let mut hasher = Sha256::new();
    for ev in evidence_list {
        hasher.update(ev.evidence_id.as_bytes());
        hasher.update(b"|");
        hasher.update(ev.gate_kind.as_bytes());
        hasher.update(b"|");
        hasher.update(ev.receipt_id.as_bytes());
        hasher.update(b"|");
        hasher.update(ev.structured_status.as_bytes());
        hasher.update(b"|");
        hasher.update(&ev.exit_code.to_le_bytes());
        hasher.update(b"|");
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Determine whether a settlement result is terminal (succeeded or failed)
/// vs retryable (infrastructure issue).
pub fn is_terminal_result(result: &SettleOutcome) -> bool {
    matches!(
        result,
        SettleOutcome::Succeeded(_)
            | SettleOutcome::CandidateFailed { .. }
            | SettleOutcome::AlreadySettled { .. }
    )
}
