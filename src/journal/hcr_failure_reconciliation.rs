use super::queue::append_event_tx;
use super::JournalStore;
use crate::domain::{JournalEventKind, RunId, RunMode};
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[cfg(not(test))]
const CLAIM_QUIESCENCE_MINUTES: i64 = 5;
#[cfg(test)]
const CLAIM_QUIESCENCE_MINUTES: i64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureReconciliation {
    pub settlement_id: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub failure_evidence_event_id: String,
    pub idempotent: bool,
}

struct FailureFact {
    error_code: String,
    event_hash: String,
}

impl JournalStore {
    /// Formally fail a quiescent HCR by reference to an immutable failure fact.
    pub fn reconcile_hcr_failure(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
        failure_event_id: &str,
    ) -> Result<FailureReconciliation> {
        validate_identity("hcr_id", hcr_id, "hcr_")?;
        validate_identity("claim_id", claim_id, "claim_")?;
        validate_identity("run_id", run_id, "run_")?;
        validate_identity("failure_event_id", failure_event_id, "event_")?;

        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        if let Some(existing) = load_existing(&tx, hcr_id)? {
            if existing.1 == claim_id
                && existing.2 == run_id
                && existing.3 == "infrastructure_failed"
                && existing.4.as_deref() == Some(failure_event_id)
            {
                tx.commit()?;
                return Ok(FailureReconciliation {
                    settlement_id: existing.0,
                    hcr_id: hcr_id.into(),
                    claim_id: claim_id.into(),
                    run_id: run_id.into(),
                    failure_evidence_event_id: failure_event_id.into(),
                    idempotent: true,
                });
            }
            bail!("HCR_FAILURE_SETTLEMENT_CONFLICT");
        }

        let hcr = load_running_hcr(&tx, hcr_id)?;
        validate_active_claim(&tx, hcr_id, claim_id, &hcr.0)?;
        validate_run_binding(&tx, hcr_id, claim_id, run_id)?;
        reject_live_kernel_work(&tx, run_id)?;
        reject_success_facts(&tx, hcr_id)?;
        let failure = validate_failure_fact(
            &tx,
            hcr_id,
            claim_id,
            run_id,
            failure_event_id,
            &hcr.1,
            &hcr.2,
            &hcr.3,
        )?;

        let now = Utc::now().to_rfc3339();
        let settlement_id = format!("stl_{}", uuid::Uuid::new_v4().simple());
        let digest = reconciliation_digest(
            hcr_id,
            claim_id,
            run_id,
            failure_event_id,
            &failure.event_hash,
        );

        cas_terminal_state(&tx, hcr_id, claim_id, run_id, &failure.error_code, &now)?;
        tx.execute(
            "INSERT INTO hcr_settlements (
                settlement_id,hcr_id,claim_id,run_id,result,error_code,
                evidence_set_digest,failure_evidence_event_id,created_at
             ) VALUES (?1,?2,?3,?4,'infrastructure_failed',?5,?6,?7,?8)",
            params![
                settlement_id,
                hcr_id,
                claim_id,
                run_id,
                failure.error_code,
                digest,
                failure_event_id,
                now,
            ],
        )?;

        let run_id_value = RunId(run_id.into());
        append_event_tx(
            &tx,
            JournalEventKind::HcrSettlementFailed,
            Some(&run_id_value),
            None,
            Some(hcr_id),
            json!({
                "hcr_id": hcr_id,
                "claim_id": claim_id,
                "run_id": run_id,
                "result": "infrastructure_failed",
                "error_code": failure.error_code,
                "evidence_set_digest": digest,
                "failure_evidence_event_id": failure_event_id,
                "settlement_id": settlement_id,
            }),
        )?;
        append_event_tx(
            &tx,
            JournalEventKind::RunFailed,
            Some(&run_id_value),
            None,
            Some(&settlement_id),
            json!({
                "run_id": run_id,
                "error_category": "hcr_failure_reconciled",
                "hcr_id": hcr_id,
                "settlement_id": settlement_id,
                "failure_evidence_event_id": failure_event_id,
            }),
        )?;
        tx.commit()?;

        Ok(FailureReconciliation {
            settlement_id,
            hcr_id: hcr_id.into(),
            claim_id: claim_id.into(),
            run_id: run_id.into(),
            failure_evidence_event_id: failure_event_id.into(),
            idempotent: false,
        })
    }
}

type ExistingSettlement = (String, String, String, String, Option<String>);
type RunningHcr = (String, String, String, String);

fn load_existing(tx: &Transaction<'_>, hcr_id: &str) -> Result<Option<ExistingSettlement>> {
    tx.query_row(
        "SELECT settlement_id,claim_id,run_id,result,failure_evidence_event_id
         FROM hcr_settlements WHERE hcr_id=?1",
        params![hcr_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_running_hcr(tx: &Transaction<'_>, hcr_id: &str) -> Result<RunningHcr> {
    let row: RunningHcr = tx
        .query_row(
            "SELECT harness_id,requirement,source_message_id,created_at
             FROM harness_change_requests WHERE request_id=?1 AND status='running'",
            params![hcr_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("HCR_FAILURE_NOT_RUNNING"))?;
    Ok(row)
}

fn validate_active_claim(
    tx: &Transaction<'_>,
    hcr_id: &str,
    claim_id: &str,
    harness_id: &str,
) -> Result<()> {
    let row: (String, String) = tx
        .query_row(
            "SELECT harness_id,claimed_at FROM hcr_claims
             WHERE hcr_id=?1 AND claim_id=?2 AND status='active'",
            params![hcr_id, claim_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("HCR_FAILURE_ACTIVE_CLAIM_NOT_FOUND"))?;
    if row.0 != harness_id {
        bail!("HCR_FAILURE_HARNESS_MISMATCH");
    }
    let claimed_at = DateTime::parse_from_rfc3339(&row.1)?.with_timezone(&Utc);
    if claimed_at > Utc::now() - Duration::minutes(CLAIM_QUIESCENCE_MINUTES) {
        bail!("HCR_FAILURE_CLAIM_NOT_QUIESCENT");
    }
    Ok(())
}

fn validate_run_binding(
    tx: &Transaction<'_>,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
) -> Result<()> {
    let row: (String, String) = tx
        .query_row(
            "SELECT status,mode FROM runs r
             JOIN hcr_run_bindings b ON b.run_id=r.id
             WHERE b.hcr_id=?1 AND b.claim_id=?2 AND b.run_id=?3",
            params![hcr_id, claim_id, run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("HCR_FAILURE_RUN_BINDING_NOT_FOUND"))?;
    if row.0 != "Running" {
        bail!("HCR_FAILURE_RUN_NOT_RUNNING");
    }
    match serde_json::from_str::<RunMode>(&row.1)? {
        RunMode::Hcr {
            hcr_id: bound_hcr,
            claim_id: bound_claim,
            ..
        } if bound_hcr == hcr_id && bound_claim == claim_id => Ok(()),
        _ => bail!("HCR_FAILURE_RUN_MODE_MISMATCH"),
    }
}

fn reject_live_kernel_work(tx: &Transaction<'_>, run_id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let jobs: i64 = tx.query_row(
        "SELECT COUNT(*) FROM worker_jobs WHERE run_id=?1 AND (
           status IN ('queued','leased','retryable_failed')
           OR (status='running' AND (locked_until IS NULL OR locked_until>?2))
         )",
        params![run_id, now],
        |row| row.get(0),
    )?;
    let dispatches: i64 = tx.query_row(
        "SELECT COUNT(*) FROM outbox_dispatches WHERE run_id=?1 AND (
           status IN ('pending','leased','retryable_failed')
           OR (status='dispatching' AND (locked_until IS NULL OR locked_until>?2))
         )",
        params![run_id, now],
        |row| row.get(0),
    )?;
    if jobs + dispatches > 0 {
        bail!("HCR_FAILURE_ACTIVE_LEASE_OR_WORK");
    }
    Ok(())
}

fn reject_success_facts(tx: &Transaction<'_>, hcr_id: &str) -> Result<()> {
    let passed: i64 = tx.query_row(
        "SELECT COUNT(*) FROM hcr_receipt_identities
         WHERE hcr_id=?1 AND overall_outcome='CandidatePassed'",
        params![hcr_id],
        |row| row.get(0),
    )?;
    let proposals: i64 = tx.query_row(
        "SELECT COUNT(*) FROM capability_proposal_hcr_links WHERE hcr_id=?1",
        params![hcr_id],
        |row| row.get(0),
    )?;
    if passed + proposals > 0 {
        bail!("HCR_FAILURE_SUCCESS_FACT_EXISTS");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_failure_fact(
    tx: &Transaction<'_>,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    failure_event_id: &str,
    requirement: &str,
    source_message_id: &str,
    hcr_created_at: &str,
) -> Result<FailureFact> {
    let event: (
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
    ) = tx
        .query_row(
            "SELECT run_id,correlation_id,kind,payload_json,hash,created_at
             FROM journal_events WHERE event_id=?1",
            params![failure_event_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("HCR_FAILURE_EVIDENCE_NOT_FOUND"))?;
    if DateTime::parse_from_rfc3339(&event.5)? < DateTime::parse_from_rfc3339(hcr_created_at)? {
        bail!("HCR_FAILURE_EVIDENCE_PREDATES_HCR");
    }
    let payload: Value = serde_json::from_str(&event.3)?;

    if gate_failure_is_bound(tx, hcr_id, claim_id, run_id, failure_event_id)? {
        return failed_receipt(&event.2, &payload, &event.4);
    }

    let requirement: Value = serde_json::from_str(requirement)?;
    let development_request_id = requirement
        .pointer("/development_request/request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("HCR_FAILURE_DEVELOPMENT_REQUEST_ID_MISSING"))?;
    let origin_run_id: String = tx
        .query_row(
            "SELECT origin_run_id FROM coding_task_submissions
             WHERE source_message_id=?1",
            params![source_message_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("HCR_FAILURE_ORIGIN_SUBMISSION_NOT_FOUND"))?;
    if event.0.as_deref() != Some(origin_run_id.as_str()) {
        bail!("HCR_FAILURE_ORIGIN_RUN_MISMATCH");
    }
    if event.2 == "RunFailed"
        && payload
            .get("development_request_id")
            .and_then(Value::as_str)
            == Some(development_request_id)
        && payload.get("run_id").and_then(Value::as_str) == Some(origin_run_id.as_str())
    {
        return Ok(FailureFact {
            error_code: required_error_code(&payload)?,
            event_hash: event.4,
        });
    }
    if event.2 == "ReceiptReceived"
        && payload.get("operation").and_then(Value::as_str) == Some("external.coding_task_submit")
    {
        let invocation_id = payload
            .get("invocation_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("HCR_FAILURE_INVOCATION_ID_MISSING"))?;
        if event.1.as_deref() == Some(invocation_id)
            && approved_invocation_is_bound(tx, &origin_run_id, invocation_id)?
        {
            return failed_receipt(&event.2, &payload, &event.4);
        }
    }
    bail!("HCR_FAILURE_EVIDENCE_NOT_TRUSTED")
}

fn approved_invocation_is_bound(
    tx: &Transaction<'_>,
    origin_run_id: &str,
    invocation_id: &str,
) -> Result<bool> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(DISTINCT kind) FROM journal_events
         WHERE run_id=?1 AND correlation_id=?2
           AND kind IN ('InvocationProposed','InvocationApproved')
           AND json_extract(payload_json,'$.operation')='external.coding_task_submit'",
        params![origin_run_id, invocation_id],
        |row| row.get(0),
    )?;
    Ok(count == 2)
}

fn gate_failure_is_bound(
    tx: &Transaction<'_>,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    event_id: &str,
) -> Result<bool> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM hcr_gate_evidence e
         JOIN hcr_gate_attempts a ON a.gate_attempt_id=e.gate_attempt_id
         WHERE a.hcr_id=?1 AND a.claim_id=?2 AND a.run_id=?3
           AND e.receipt_event_id=?4",
        params![hcr_id, claim_id, run_id, event_id],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn failed_receipt(kind: &str, payload: &Value, event_hash: &str) -> Result<FailureFact> {
    if kind != "ReceiptReceived" || payload.get("status").and_then(Value::as_str) != Some("Failed")
    {
        bail!("HCR_FAILURE_RECEIPT_NOT_FAILED");
    }
    Ok(FailureFact {
        error_code: required_error_code(payload.get("output").unwrap_or(payload))?,
        event_hash: event_hash.into(),
    })
}

fn required_error_code(payload: &Value) -> Result<String> {
    let code = payload
        .get("detail_code")
        .or_else(|| payload.get("error_category"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("HCR_FAILURE_ERROR_CODE_MISSING"))?;
    if code.is_empty()
        || code.len() > 128
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("HCR_FAILURE_ERROR_CODE_INVALID");
    }
    Ok(code.into())
}

fn cas_terminal_state(
    tx: &Transaction<'_>,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    error_code: &str,
    now: &str,
) -> Result<()> {
    if tx.execute(
        "UPDATE harness_change_requests
         SET status='failed',error_code=?1,updated_at=?2
         WHERE request_id=?3 AND status='running'",
        params![error_code, now, hcr_id],
    )? != 1
        || tx.execute(
            "UPDATE hcr_claims SET status='released'
             WHERE claim_id=?1 AND hcr_id=?2 AND status='active'",
            params![claim_id, hcr_id],
        )? != 1
        || tx.execute(
            "UPDATE runs SET status='Failed',updated_at=?1
             WHERE id=?2 AND status='Running'",
            params![now, run_id],
        )? != 1
    {
        bail!("HCR_FAILURE_TERMINAL_CAS_CONFLICT");
    }
    Ok(())
}

fn reconciliation_digest(
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    event_id: &str,
    event_hash: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [hcr_id, claim_id, run_id, event_id, event_hash] {
        hasher.update(part.as_bytes());
        hasher.update(b"|");
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn validate_identity(name: &str, value: &str, prefix: &str) -> Result<()> {
    if !value.starts_with(prefix)
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("HCR_FAILURE_INVALID_{name}");
    }
    Ok(())
}
