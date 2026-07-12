//! HCR gate attempt, evidence, and settlement persistence (R3A-R1).
//! Store methods for hcr_gate_attempts, hcr_gate_evidence, hcr_settlements.

use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::json;

impl JournalStore {
    // ── Gate Attempt ──────────────────────────────────────────────────

    pub fn insert_gate_attempt(
        &self,
        id: &str,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
        harness_id: &str,
        workspace_id: &str,
        gate_kind: &str,
        expected_op: &str,
        expected_profile: &str,
        intent_id: &str,
        created_at: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        conn.execute(
            "INSERT INTO hcr_gate_attempts
             (gate_attempt_id, hcr_id, claim_id, run_id, harness_id, workspace_id,
              gate_kind, expected_operation, expected_profile, invocation_intent_id, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                id,
                hcr_id,
                claim_id,
                run_id,
                harness_id,
                workspace_id,
                gate_kind,
                expected_op,
                expected_profile,
                intent_id,
                created_at
            ],
        )?;
        Ok(())
    }

    pub fn get_gate_attempt(&self, id: &str) -> Result<Option<HcrGateAttempt>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        conn.query_row(
            "SELECT gate_attempt_id, hcr_id, claim_id, run_id, harness_id, workspace_id,
                    gate_kind, expected_operation, expected_profile, invocation_intent_id, created_at
             FROM hcr_gate_attempts WHERE gate_attempt_id = ?1",
            params![id],
            |row| Ok(HcrGateAttempt {
                gate_attempt_id: row.get(0)?, hcr_id: row.get(1)?,
                claim_id: row.get(2)?, run_id: row.get(3)?,
                harness_id: row.get(4)?, workspace_id: row.get(5)?,
                gate_kind: row.get(6)?, expected_operation: row.get(7)?,
                expected_profile: row.get(8)?, invocation_intent_id: row.get(9)?,
                created_at: row.get(10)?,
            }),
        ).optional().map_err(Into::into)
    }

    pub fn get_attempts_for_hcr(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
    ) -> Result<Vec<HcrGateAttempt>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        let mut stmt = conn.prepare(
            "SELECT gate_attempt_id, hcr_id, claim_id, run_id, harness_id, workspace_id,
                    gate_kind, expected_operation, expected_profile, invocation_intent_id, created_at
             FROM hcr_gate_attempts
             WHERE hcr_id = ?1 AND claim_id = ?2 AND run_id = ?3
             ORDER BY gate_kind"
        )?;
        let rows = stmt.query_map(params![hcr_id, claim_id, run_id], |row| {
            Ok(HcrGateAttempt {
                gate_attempt_id: row.get(0)?,
                hcr_id: row.get(1)?,
                claim_id: row.get(2)?,
                run_id: row.get(3)?,
                harness_id: row.get(4)?,
                workspace_id: row.get(5)?,
                gate_kind: row.get(6)?,
                expected_operation: row.get(7)?,
                expected_profile: row.get(8)?,
                invocation_intent_id: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?;
        let mut r = Vec::new();
        for row in rows {
            r.push(row?);
        }
        Ok(r)
    }

    // ── Evidence ──────────────────────────────────────────────────────

    pub fn insert_evidence_atomically(
        &self,
        id: &str,
        attempt_id: &str,
        receipt_event_id: &str,
        payload_digest: &str,
        created_at: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let updated = tx.execute(
            "INSERT OR IGNORE INTO hcr_gate_evidence
             (evidence_id, gate_attempt_id, receipt_event_id, receipt_payload_digest, created_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![id, attempt_id, receipt_event_id, payload_digest, created_at],
        )?;
        if updated == 0 {
            tx.commit()?;
            anyhow::bail!("EVIDENCE_INSERT_FAILED: duplicate or constraint violation");
        }

        let event_id = EventId::new();
        let kind_text = format!("{:?}", JournalEventKind::HcrEvidenceRegistered);
        let payload_json = serde_json::to_string(&json!({
            "evidence_id": id, "gate_attempt_id": attempt_id,
            "receipt_event_id": receipt_event_id,
        }))?;

        let prev: Option<(i64, String)> = tx
            .query_row(
                "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let seq = prev.as_ref().map(|(s, _)| s + 1).unwrap_or(1);
        let prev_hash = prev.map(|(_, h)| h);
        let hash =
            super::hash_chain::event_hash(prev_hash.as_deref(), seq, &kind_text, &payload_json);

        tx.execute(
            "INSERT INTO journal_events (sequence,event_id,run_id,session_id,correlation_id,kind,payload_json,previous_hash,hash,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![seq, event_id.0, Option::<&str>::None, Option::<&str>::None,
                    Some(id), kind_text, payload_json, prev_hash, hash, created_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_evidence_for_attempts(&self, attempt_ids: &[&str]) -> Result<Vec<HcrGateEvidence>> {
        if attempt_ids.is_empty() {
            return Ok(vec![]);
        }
        let placeholders: Vec<String> = attempt_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT evidence_id, gate_attempt_id, receipt_event_id, receipt_payload_digest, created_at
             FROM hcr_gate_evidence WHERE gate_attempt_id IN ({})",
            placeholders.join(","),
        );
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = attempt_ids
            .iter()
            .map(|s| Box::new(s.to_string()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(HcrGateEvidence {
                evidence_id: row.get(0)?,
                gate_attempt_id: row.get(1)?,
                receipt_event_id: row.get(2)?,
                receipt_payload_digest: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        let mut r = Vec::new();
        for row in rows {
            r.push(row?);
        }
        Ok(r)
    }

    // ── Terminal Settlement ──────────────────────────────────────────

    /// Write a terminal settlement from pre-validated source facts.
    /// Computes result and digest internally — caller provides only identity
    /// keys and validated gate data. No caller-supplied result/status/digest.
    pub(crate) fn settle_hcr_terminal(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
        error_code: Option<&str>,
        attempts: &[HcrGateAttempt],
        parsed_receipts: &[ValidatedGateReceipt],
    ) -> Result<String> {
        // Classify result from source facts.
        let has_failure = parsed_receipts
            .iter()
            .any(|r| r.exit_code != 0 || r.status == "Failed");
        let result = if has_failure {
            "candidate_failed"
        } else {
            "succeeded"
        };
        // HCR status column uses 'failed' (CHECK: pending/running/succeeded/failed/cancelled).
        // Settlement record uses 'candidate_failed' (CHECK: succeeded/candidate_failed).

        // Compute canonical digest from source facts.
        let digest = crate::hcr::settlement::compute_digest(
            hcr_id,
            claim_id,
            run_id,
            attempts,
            parsed_receipts,
        );

        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;

        // Check existing settlement with digest comparison.
        if let Some(existing) = conn
            .query_row(
                "SELECT settlement_id, evidence_set_digest FROM hcr_settlements WHERE hcr_id = ?1",
                params![hcr_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if existing.1 != digest {
                anyhow::bail!(
                    "SETTLE_DIGEST_CONFLICT: existing {} != {}",
                    existing.1,
                    digest
                );
            }
            return Ok(existing.0);
        }

        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // HCR status uses 'failed' for candidate failures (CHECK constraint).
        let hcr_status = if has_failure { "failed" } else { "succeeded" };
        let updated = tx.execute(
            "UPDATE harness_change_requests SET status = ?1, error_code = ?2, updated_at = ?3
             WHERE request_id = ?4 AND status = 'running'",
            params![
                hcr_status,
                error_code,
                chrono::Utc::now().to_rfc3339(),
                hcr_id
            ],
        )?;
        if updated == 0 {
            tx.commit()?;
            if let Some(sid) = conn
                .query_row(
                    "SELECT settlement_id FROM hcr_settlements WHERE hcr_id = ?1",
                    params![hcr_id],
                    |row| row.get(0),
                )
                .optional()?
            {
                return Ok(sid);
            }
            anyhow::bail!("SETTLE_CAS_FAILED: HCR {hcr_id} not running");
        }

        let settlement_id = format!("stl_{}", uuid::Uuid::new_v4().simple());
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO hcr_settlements (settlement_id, hcr_id, claim_id, run_id, result, error_code, evidence_set_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![settlement_id, hcr_id, claim_id, run_id, result, error_code, digest, now],
        )?;

        // Terminal journal event.
        let terminal_kind = if result == "succeeded" {
            JournalEventKind::HcrSettlementSucceeded
        } else {
            JournalEventKind::HcrSettlementFailed
        };
        let ev_id = EventId::new();
        let kind_text = format!("{:?}", terminal_kind);
        let payload_json = serde_json::to_string(&json!({
            "hcr_id": hcr_id, "claim_id": claim_id, "run_id": run_id,
            "result": result, "error_code": error_code,
            "evidence_set_digest": digest, "settlement_id": settlement_id,
        }))?;

        let prev: Option<(i64, String)> = tx
            .query_row(
                "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let seq = prev.as_ref().map(|(s, _)| s + 1).unwrap_or(1);
        let prev_hash = prev.map(|(_, h)| h);
        let hash =
            super::hash_chain::event_hash(prev_hash.as_deref(), seq, &kind_text, &payload_json);
        tx.execute(
            "INSERT INTO journal_events (sequence,event_id,run_id,session_id,correlation_id,kind,payload_json,previous_hash,hash,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![seq, ev_id.0, Option::<&str>::None, Option::<&str>::None,
                    hcr_id, kind_text, payload_json, prev_hash, hash, now],
        )?;
        tx.commit()?;
        Ok(settlement_id)
    }

    // ── Run loading ──────────────────────────────────────────────────

    pub fn get_run(&self, run_id: &str) -> Result<Option<Run>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        let row = conn
            .query_row(
                "SELECT id, session_id, agent_id, trigger_event_id, principal_json,
                    parent_run_id, delegated_by, status, created_at, updated_at,
                    registry_snapshot_id, mode
             FROM runs WHERE id = ?1",
                params![run_id],
                |row| {
                    let mode_str: String = row.get(11)?;
                    let mode: RunMode = serde_json::from_str(&mode_str).unwrap_or(RunMode::Default);
                    Ok(Run {
                        id: RunId(row.get(0)?),
                        session_id: SessionId(row.get(1)?),
                        agent_id: AgentId(row.get(2)?),
                        trigger_event_id: EventId(row.get(3)?),
                        principal: serde_json::from_str(&row.get::<_, String>(4)?)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                        parent_run_id: row.get::<_, Option<String>>(5)?.map(RunId),
                        delegated_by: row.get::<_, Option<String>>(6)?.map(PrincipalId),
                        status: RunStatus::Running, // simplified
                        created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                            .with_timezone(&chrono::Utc),
                        updated_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                            .with_timezone(&chrono::Utc),
                        registry_snapshot_id: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                        mode,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    // ── Evidence list ────────────────────────────────────────────────

    pub fn get_gate_evidence_for_hcr(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
    ) -> Result<Vec<HcrGateEvidence>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        let mut stmt = conn.prepare(
            "SELECT e.evidence_id, e.gate_attempt_id, e.receipt_event_id,
                    e.receipt_payload_digest, e.created_at
             FROM hcr_gate_evidence e
             JOIN hcr_gate_attempts a ON e.gate_attempt_id = a.gate_attempt_id
             WHERE a.hcr_id = ?1 AND a.claim_id = ?2 AND a.run_id = ?3
             ORDER BY a.gate_kind",
        )?;
        let rows = stmt.query_map(params![hcr_id, claim_id, run_id], |row| {
            Ok(HcrGateEvidence {
                evidence_id: row.get(0)?,
                gate_attempt_id: row.get(1)?,
                receipt_event_id: row.get(2)?,
                receipt_payload_digest: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        let mut r = Vec::new();
        for row in rows {
            r.push(row?);
        }
        Ok(r)
    }

    pub fn get_settlement(&self, hcr_id: &str) -> Result<Option<HcrSettlement>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;
        conn.query_row(
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
        .optional()
        .map_err(Into::into)
    }
}
