//! Evidence-based HCR execution recovery (R3A).
//!
//! Determines execution progress from persistent state only:
//! - HCR status, claim, run binding
//! - Durable gate evidence records
//! - Settlement record
//!
//! Does NOT trust event names (HcrGateCompleted, etc.) to determine
//! gate completion. Only canonical `hcr_gate_evidence` rows count.

use crate::domain::*;
use crate::hcr::evidence::{self, check_gate_completeness, validate_gate_evidence};
use crate::journal::JournalStore;
use anyhow::Result;

/// Resume state derived from durable evidence only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeState {
    /// No claim exists yet — start fresh.
    NotStarted,
    /// Claim exists, no Run binding — create binding.
    ClaimedNoBinding { claim_id: String },
    /// Binding exists, Runtime may or may not have started.
    /// We can't determine Runtime progress from evidence alone at this layer.
    /// The caller checks Runtime state separately.
    Bound { claim_id: String, run_id: String },
    /// All five required gates have durable evidence and pass validation.
    AllGatesCompleted { claim_id: String, run_id: String },
    /// HCR has a terminal settlement record.
    AlreadySettled {
        settlement_id: String,
        result: String,
    },
}

/// Determine resume state from persistent store only.
///
/// Does NOT trust journal event names. Only `hcr_gate_evidence` rows
/// and the `hcr_settlements` table are authoritative.
pub fn determine_resume_state(journal: &JournalStore, hcr_id: &str) -> Result<ResumeState> {
    // ── 1. First check for settlement (fast path) ─────────────────────
    let hcr = journal.get_harness_change_request(hcr_id)?;

    // Check settlement record (most authoritative).
    if let Some(settlement) = journal.get_hcr_settlement(hcr_id)? {
        return Ok(ResumeState::AlreadySettled {
            settlement_id: settlement.settlement_id,
            result: settlement.result,
        });
    }

    // If HCR status is terminal but no settlement record exists, that's a
    // corruption state. Report as AlreadySettled with the status.
    if let Some(ref hcr) = hcr {
        if hcr.status == "succeeded" || hcr.status == "failed" {
            return Ok(ResumeState::AlreadySettled {
                settlement_id: String::new(),
                result: hcr.status.clone(),
            });
        }
    }

    // ── 2. Check for claim ────────────────────────────────────────────
    let claim = match journal.get_active_claim_for_hcr(hcr_id)? {
        Some(c) => c,
        None => return Ok(ResumeState::NotStarted),
    };

    // ── 3. Check for Run binding ──────────────────────────────────────
    let binding = match journal.get_run_binding_for_claim(&claim.claim_id.0)? {
        Some(b) => b,
        None => {
            return Ok(ResumeState::ClaimedNoBinding {
                claim_id: claim.claim_id.0,
            });
        }
    };

    // ── 4. Check for durable gate evidence (authoritative) ────────────
    let evidence_list =
        evidence::load_gate_evidence(journal, hcr_id, &claim.claim_id.0, &binding.run_id)?;

    // Check completeness first (must have exactly 5 distinct gates).
    if check_gate_completeness(&evidence_list).is_ok() {
        // All 5 gates have evidence. Now validate each one.
        let all_valid = evidence_list
            .iter()
            .all(|ev| validate_gate_evidence(ev).is_ok());

        if all_valid {
            return Ok(ResumeState::AllGatesCompleted {
                claim_id: claim.claim_id.0,
                run_id: binding.run_id,
            });
        }
    }

    // ── 5. Default: bound but gates incomplete ────────────────────────
    Ok(ResumeState::Bound {
        claim_id: claim.claim_id.0,
        run_id: binding.run_id,
    })
}
