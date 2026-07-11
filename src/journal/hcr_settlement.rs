//! Durable HCR gate evidence and settlement persistence (R3A).
//!
//! Store methods for the `hcr_gate_evidence` and `hcr_settlements` tables.
//! Extracted to a separate file to keep `harness_change_requests.rs` under
//! the 500-line structure limit.

use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
use rusqlite::params;
use rusqlite::OptionalExtension;

impl JournalStore {
    /// Insert a gate evidence record. All fields are pre-validated by the
    /// caller (`register_gate_evidence`). The UNIQUE constraints on
    /// (hcr_id, claim_id, run_id, gate_kind), receipt_id, and
    /// invocation_intent_id prevent duplicates.
    pub fn insert_gate_evidence(
        &self,
        evidence_id: &str,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
        harness_id: &str,
        workspace_id: &str,
        gate_kind: &str,
        invocation_intent_id: &str,
        receipt_id: &str,
        operation: &str,
        execution_profile: &str,
        structured_status: &str,
        exit_code: i32,
        timed_out: bool,
        stdout_truncated: bool,
        stderr_truncated: bool,
        child_cleanup: Option<bool>,
        error_code: Option<&str>,
        artifact_digest: Option<&str>,
        manifest_digest: Option<&str>,
        created_at: &str,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        conn.execute(
            "INSERT OR IGNORE INTO hcr_gate_evidence
             (evidence_id, hcr_id, claim_id, run_id, harness_id, workspace_id,
              gate_kind, invocation_intent_id, receipt_id, operation, execution_profile,
              structured_status, exit_code, timed_out, stdout_truncated, stderr_truncated,
              child_cleanup, error_code, artifact_digest, manifest_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
            params![
                evidence_id,
                hcr_id,
                claim_id,
                run_id,
                harness_id,
                workspace_id,
                gate_kind,
                invocation_intent_id,
                receipt_id,
                operation,
                execution_profile,
                structured_status,
                exit_code,
                timed_out as i32,
                stdout_truncated as i32,
                stderr_truncated as i32,
                child_cleanup,
                error_code,
                artifact_digest,
                manifest_digest,
                created_at,
            ],
        )?;

        Ok(())
    }

    /// Load all gate evidence records for an HCR/claim/run.
    pub fn get_gate_evidence_for_hcr(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
    ) -> Result<Vec<HcrGateEvidence>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT evidence_id, hcr_id, claim_id, run_id, harness_id, workspace_id,
                    gate_kind, invocation_intent_id, receipt_id, operation, execution_profile,
                    structured_status, exit_code, timed_out, stdout_truncated, stderr_truncated,
                    child_cleanup, error_code, artifact_digest, manifest_digest, created_at
             FROM hcr_gate_evidence
             WHERE hcr_id = ?1 AND claim_id = ?2 AND run_id = ?3
             ORDER BY gate_kind",
        )?;

        let rows = stmt.query_map(params![hcr_id, claim_id, run_id], |row| {
            Ok(HcrGateEvidence {
                evidence_id: row.get(0)?,
                hcr_id: row.get(1)?,
                claim_id: row.get(2)?,
                run_id: row.get(3)?,
                harness_id: row.get(4)?,
                workspace_id: row.get(5)?,
                gate_kind: row.get(6)?,
                invocation_intent_id: row.get(7)?,
                receipt_id: row.get(8)?,
                operation: row.get(9)?,
                execution_profile: row.get(10)?,
                structured_status: row.get(11)?,
                exit_code: row.get(12)?,
                timed_out: row.get::<_, i32>(13)? != 0,
                stdout_truncated: row.get::<_, i32>(14)? != 0,
                stderr_truncated: row.get::<_, i32>(15)? != 0,
                child_cleanup: row.get(16)?,
                error_code: row.get(17)?,
                artifact_digest: row.get(18)?,
                manifest_digest: row.get(19)?,
                created_at: row.get(20)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Atomically settle an HCR: update status, write settlement record, and
    /// append terminal journal event — all in one SQLite transaction.
    ///
    /// Returns the settlement_id. Idempotent: if already settled, returns
    /// the existing settlement_id without changes.
    pub fn settle_hcr_atomically(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
        result: &str,
        error_code: Option<&str>,
        evidence_set_digest: &str,
    ) -> Result<String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        // Check for existing settlement first.
        let existing: Option<String> = conn
            .query_row(
                "SELECT settlement_id FROM hcr_settlements WHERE hcr_id = ?1",
                params![hcr_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(sid) = existing {
            return Ok(sid);
        }

        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // 1. Verify HCR is still running (CAS).
        let updated = tx.execute(
            "UPDATE harness_change_requests
             SET status = ?1, error_code = ?2, updated_at = ?3
             WHERE request_id = ?4 AND status = 'running'",
            params![result, error_code, chrono::Utc::now().to_rfc3339(), hcr_id],
        )?;

        if updated == 0 {
            // Either already settled or not running — rollback and check.
            tx.commit()?; // Release the transaction.
            let existing: Option<String> = conn
                .query_row(
                    "SELECT settlement_id FROM hcr_settlements WHERE hcr_id = ?1",
                    params![hcr_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(sid) = existing {
                return Ok(sid);
            }
            anyhow::bail!(
                "SETTLE_CAS_FAILED: HCR {hcr_id} status is not running or already terminal"
            );
        }

        // 2. Create settlement record.
        let settlement_id = format!("stl_{}", uuid::Uuid::new_v4().simple());
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO hcr_settlements
             (settlement_id, hcr_id, claim_id, run_id, result, error_code, evidence_set_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                settlement_id,
                hcr_id,
                claim_id,
                run_id,
                result,
                error_code,
                evidence_set_digest,
                now,
            ],
        )?;

        // 3. Write terminal journal event in the same transaction.
        let terminal_kind = if result == "succeeded" {
            JournalEventKind::HcrSettlementSucceeded
        } else {
            JournalEventKind::HcrSettlementFailed
        };

        // We need to append the event within the transaction.
        let event_id = crate::domain::EventId::new();
        let kind_text = format!("{:?}", terminal_kind);
        let payload_json = serde_json::to_string(&serde_json::json!({
            "hcr_id": hcr_id,
            "claim_id": claim_id,
            "run_id": run_id,
            "result": result,
            "error_code": error_code,
            "evidence_set_digest": evidence_set_digest,
            "settlement_id": settlement_id,
        }))?;

        let previous: Option<(i64, String)> = tx
            .query_row(
                "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let sequence = previous.as_ref().map(|(seq, _)| seq + 1).unwrap_or(1);
        let previous_hash = previous.map(|(_, hash)| hash);
        let hash = super::hash_chain::event_hash(
            previous_hash.as_deref(),
            sequence,
            &kind_text,
            &payload_json,
        );

        tx.execute(
            "INSERT INTO journal_events
             (sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                sequence,
                event_id.0,
                Option::<&str>::None,     // run_id
                Option::<&str>::None,     // session_id
                hcr_id,                   // correlation_id
                kind_text,
                payload_json,
                previous_hash,
                hash,
                now,
            ],
        )?;

        // 4. Commit everything atomically.
        tx.commit()?;

        Ok(settlement_id)
    }

    /// Load a settlement record for an HCR.
    pub fn get_hcr_settlement(&self, hcr_id: &str) -> Result<Option<HcrSettlement>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row = conn
            .query_row(
                "SELECT settlement_id, hcr_id, claim_id, run_id, result, error_code,
                        evidence_set_digest, created_at
                 FROM hcr_settlements WHERE hcr_id = ?1",
                params![hcr_id],
                |row| {
                    Ok(HcrSettlement {
                        settlement_id: row.get(0)?,
                        hcr_id: row.get(1)?,
                        claim_id: row.get(2)?,
                        run_id: row.get(3)?,
                        result: row.get(4)?,
                        error_code: row.get(5)?,
                        evidence_set_digest: row.get(6)?,
                        created_at: row.get(7)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}
