//! Atomic HCR settlement — everything validated from source facts (R3A-R2).

use crate::domain::*;
use crate::hcr::validate;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

pub fn settle_hcr(
    journal: &JournalStore,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
) -> Result<SettlementResult> {
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_HCR_NOT_FOUND"))?;
    if hcr.status == "succeeded" || hcr.status == "failed" {
        return if let Some(stl) = journal.get_settlement(hcr_id)? {
            Ok(settlement_to_result(&stl))
        } else {
            Ok(SettlementResult::EvidenceIncomplete(
                "terminal no record".into(),
            ))
        };
    }
    if hcr.status != "running" {
        bail!("SETTLE_HCR_NOT_RUNNING");
    }

    let claim = journal
        .get_active_claim_for_hcr(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_NO_CLAIM"))?;
    if claim.claim_id.0 != *claim_id {
        bail!("SETTLE_CLAIM_MISMATCH");
    }
    let binding = journal
        .get_run_binding_for_claim(claim_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_NO_BINDING"))?;
    if binding.run_id != *run_id {
        bail!("SETTLE_RUN_MISMATCH");
    }
    let run = journal
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("SETTLE_RUN_NOT_FOUND"))?;
    match &run.mode {
        RunMode::Hcr {
            hcr_id: rh,
            claim_id: rc,
            ..
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

    let attempts = journal.get_attempts_for_hcr(hcr_id, claim_id, run_id)?;
    if attempts.len() != 5 {
        return Ok(SettlementResult::EvidenceIncomplete(format!(
            "attempts {}",
            attempts.len()
        )));
    }
    let kinds: Vec<&str> = attempts.iter().map(|a| a.gate_kind.as_str()).collect();
    let required: Vec<&str> = GateKind::all_required()
        .iter()
        .map(|k| k.as_str())
        .collect();
    if kinds != required {
        return Ok(SettlementResult::EvidenceIncomplete(format!(
            "kinds {:?} vs {:?}",
            kinds, required
        )));
    }

    let a_ids: Vec<&str> = attempts
        .iter()
        .map(|a| a.gate_attempt_id.as_str())
        .collect();
    let ev_list = journal.get_evidence_for_attempts(&a_ids)?;
    if ev_list.len() != 5 {
        return Ok(SettlementResult::EvidenceIncomplete(format!(
            "evidence {}",
            ev_list.len()
        )));
    }

    let mut infra = false;
    let mut candidate = false;
    let mut first_err = String::new();
    let mut parsed: Vec<ValidatedGateReceipt> = Vec::new();

    for ev in &ev_list {
        match validate::validate_gate_source_chain(journal, &ev.gate_attempt_id) {
            Ok(p) => {
                if ev.receipt_event_id != p.receipt_event_id {
                    infra = true;
                    first_err = "receipt event mismatch".into();
                    continue;
                }
                if ev.receipt_payload_digest != p.receipt_payload_digest {
                    infra = true;
                    first_err = "digest mismatch".into();
                    continue;
                }
                parsed.push(p);
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("VALIDATE_NO_RECEIPT")
                    || msg.contains("VALIDATE_CONFLICTING_RECEIPTS")
                {
                    infra = true;
                } else {
                    candidate = true;
                }
                first_err = msg;
            }
        }
    }

    for p in &parsed {
        if p.timed_out {
            infra = true;
            first_err = format!("{} timed out", p.gate_attempt_id);
            continue;
        }
        if p.child_cleanup != Some(true) {
            infra = true;
            first_err = format!("{} cleanup={:?}", p.gate_attempt_id, p.child_cleanup);
            continue;
        }
        if p.error_code.is_some() {
            infra = true;
            first_err = format!("{} error={:?}", p.gate_attempt_id, p.error_code);
            continue;
        }
        if p.status == "Failed" || p.exit_code != 0 {
            candidate = true;
            first_err = format!(
                "{} failed st={} exit={}",
                p.gate_attempt_id, p.status, p.exit_code
            );
            continue;
        }
    }

    if infra {
        return Ok(SettlementResult::InfrastructureFailure(first_err));
    }
    if parsed.len() != 5 {
        return Ok(SettlementResult::EvidenceIncomplete(format!(
            "{} of 5 validated",
            parsed.len()
        )));
    }

    let digest = compute_digest(hcr_id, claim_id, run_id, &attempts, &parsed);

    if !candidate {
        match journal.settle_hcr_terminal_internal(
            hcr_id,
            claim_id,
            run_id,
            "succeeded",
            None,
            &digest,
        ) {
            Ok(sid) => Ok(SettlementResult::Succeeded(sid)),
            Err(_) => check_conflict(journal, hcr_id, &digest),
        }
    } else {
        match journal.settle_hcr_terminal_internal(
            hcr_id,
            claim_id,
            run_id,
            "candidate_failed",
            Some(&first_err),
            &digest,
        ) {
            Ok(sid) => Ok(SettlementResult::CandidateFailed(sid)),
            Err(_) => check_conflict(journal, hcr_id, &digest),
        }
    }
}

fn compute_digest(
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    attempts: &[HcrGateAttempt],
    parsed: &[ValidatedGateReceipt],
) -> String {
    let mut h = Sha256::new();
    h.update(hcr_id.as_bytes());
    h.update(b"|");
    h.update(claim_id.as_bytes());
    h.update(b"|");
    h.update(run_id.as_bytes());
    h.update(b"|");
    for kind in GateKind::all_required() {
        h.update(kind.as_str().as_bytes());
        h.update(b"|");
        if let Some(a) = attempts.iter().find(|a| a.gate_kind == kind.as_str()) {
            h.update(a.gate_attempt_id.as_bytes());
            h.update(b"|");
            h.update(a.invocation_intent_id.as_bytes());
            h.update(b"|");
            h.update(a.expected_operation.as_bytes());
            h.update(b"|");
            h.update(a.expected_profile.as_bytes());
            h.update(b"|");
            h.update(a.workspace_id.as_bytes());
            h.update(b"|");
            h.update(a.harness_id.as_bytes());
            h.update(b"|");
        }
        if let Some(p) = parsed.iter().find(|p| {
            attempts
                .iter()
                .any(|a| a.gate_kind == kind.as_str() && a.gate_attempt_id == p.gate_attempt_id)
        }) {
            h.update(p.receipt_event_id.as_bytes());
            h.update(b"|");
            h.update(p.receipt_payload_digest.as_bytes());
            h.update(b"|");
            h.update(p.status.as_bytes());
            h.update(b"|");
            h.update(&p.exit_code.to_le_bytes());
            h.update(b"|");
            h.update(p.timed_out.to_string().as_bytes());
            h.update(b"|");
            h.update(
                p.child_cleanup
                    .map(|c| c.to_string())
                    .unwrap_or_default()
                    .as_bytes(),
            );
            h.update(b"|");
            h.update(p.error_code.as_deref().unwrap_or("").as_bytes());
            h.update(b"|");
        }
    }
    format!("sha256:{}", hex::encode(h.finalize()))
}

fn check_conflict(journal: &JournalStore, hcr_id: &str, digest: &str) -> Result<SettlementResult> {
    if let Some(stl) = journal.get_settlement(hcr_id)? {
        if stl.evidence_set_digest != digest {
            return Ok(SettlementResult::EvidenceConflict(format!(
                "existing digest {} != {}",
                stl.evidence_set_digest, digest
            )));
        }
        Ok(settlement_to_result(&stl))
    } else {
        Ok(SettlementResult::EvidenceConflict(
            "CAS failed, no settlement".into(),
        ))
    }
}

fn settlement_to_result(stl: &HcrSettlement) -> SettlementResult {
    match stl.result.as_str() {
        "succeeded" => SettlementResult::Succeeded(stl.settlement_id.clone()),
        "candidate_failed" => SettlementResult::CandidateFailed(stl.settlement_id.clone()),
        _ => SettlementResult::InfrastructureFailure(format!("unknown {}", stl.result)),
    }
}
