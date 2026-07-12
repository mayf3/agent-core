//! Evidence-based HCR recovery (R3A-R1).
//! Checks terminal triple consistency: HCR status + settlement + event.
//! Gates verified via canonical attempts + evidence + receipt events.

use crate::domain::*;
use crate::hcr::evidence;
use crate::journal::JournalStore;
use anyhow::Result;

pub enum ResumeState {
    NotStarted,
    ClaimedNoBinding {
        claim_id: String,
    },
    Bound {
        claim_id: String,
        run_id: String,
    },
    AllGatesCompleted {
        claim_id: String,
        run_id: String,
    },
    AlreadySettled {
        settlement_id: String,
        result: String,
    },
    Corruption(String),
}

pub fn determine_resume_state(journal: &JournalStore, hcr_id: &str) -> Result<ResumeState> {
    let hcr = journal.get_harness_change_request(hcr_id)?;
    let settlement = journal.get_settlement(hcr_id)?;
    let events = journal.events()?;

    let terminal_events: Vec<&JournalEvent> = events
        .iter()
        .filter(|e| {
            (e.kind == JournalEventKind::HcrSettlementSucceeded
                || e.kind == JournalEventKind::HcrSettlementFailed)
                && e.correlation_id.as_deref() == Some(hcr_id)
        })
        .collect();

    let hcr_terminal = hcr
        .as_ref()
        .map_or(false, |h| h.status == "succeeded" || h.status == "failed");

    // Triple consistency check.
    if hcr_terminal || settlement.is_some() || !terminal_events.is_empty() {
        match (&hcr, &settlement, terminal_events.len()) {
            (Some(h), Some(s), 1) if (h.status == "succeeded" || h.status == "failed") => {
                let ev = terminal_events[0];
                let ev_result = ev
                    .payload
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if h.status == ev_result && s.result == ev_result && s.hcr_id == hcr_id {
                    return Ok(ResumeState::AlreadySettled {
                        settlement_id: s.settlement_id.clone(),
                        result: s.result.clone(),
                    });
                }
                return Ok(ResumeState::Corruption(format!(
                    "triple mismatch: HCR={}, settlement={}, event={}",
                    h.status, s.result, ev_result,
                )));
            }
            (Some(h), _, 0) if hcr_terminal => {
                return Ok(ResumeState::Corruption(format!(
                    "HCR terminal {} no settlement",
                    h.status
                )));
            }
            (_, Some(s), 0) => {
                return Ok(ResumeState::Corruption(format!(
                    "settlement {} no event",
                    s.result
                )));
            }
            (_, _, n) if n > 1 => {
                return Ok(ResumeState::Corruption(format!("{} terminal events", n)))
            }
            _ => {}
        }
    }

    // Check claim.
    let Some(claim) = journal.get_active_claim_for_hcr(hcr_id)? else {
        return Ok(ResumeState::NotStarted);
    };
    let Some(binding) = journal.get_run_binding_for_claim(&claim.claim_id.0)? else {
        return Ok(ResumeState::ClaimedNoBinding {
            claim_id: claim.claim_id.0,
        });
    };

    // Check attempts + evidence (authoritative).
    let attempts = journal.get_attempts_for_hcr(hcr_id, &claim.claim_id.0, &binding.run_id)?;
    if attempts.len() != 5 {
        return Ok(ResumeState::Bound {
            claim_id: claim.claim_id.0,
            run_id: binding.run_id,
        });
    }
    let evidence_list = journal.get_evidence_for_attempts(
        &attempts
            .iter()
            .map(|a| a.gate_attempt_id.as_str())
            .collect::<Vec<_>>(),
    )?;
    if evidence_list.len() != 5 {
        return Ok(ResumeState::Bound {
            claim_id: claim.claim_id.0,
            run_id: binding.run_id,
        });
    }

    for ev in &evidence_list {
        let Some(re) = events.iter().find(|e| e.event_id.0 == ev.receipt_event_id) else {
            return Ok(ResumeState::Bound {
                claim_id: claim.claim_id.0,
                run_id: binding.run_id,
            });
        };
        if re.kind != JournalEventKind::ReceiptReceived {
            return Ok(ResumeState::Bound {
                claim_id: claim.claim_id.0,
                run_id: binding.run_id,
            });
        }
        if evidence::verify_evidence_against_receipt(ev, re).is_err() {
            return Ok(ResumeState::Bound {
                claim_id: claim.claim_id.0,
                run_id: binding.run_id,
            });
        }
    }

    Ok(ResumeState::AllGatesCompleted {
        claim_id: claim.claim_id.0,
        run_id: binding.run_id,
    })
}
