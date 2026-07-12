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

    // ── Terminal Settlement (identity-only, all inside transaction) ────

    /// The ONLY production settlement entry point. Accepts only identity keys.
    /// All source facts are loaded and validated inside a single BEGIN IMMEDIATE
    /// transaction. No caller-supplied attempts, parsed_receipts, result, or
    /// digest are accepted.
    pub(crate) fn settle_hcr_in_tx(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
    ) -> Result<SettlementResult> {
        use super::sqlite_read::parse_kind;
        use sha2::{Digest, Sha256};

        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex"))?;

        // Check existing settlement first (fast path, not security critical).
        if let Some(existing) = conn
            .query_row(
                "SELECT settlement_id, evidence_set_digest FROM hcr_settlements WHERE hcr_id = ?1",
                params![hcr_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            existing.0.clone();
            return Ok(SettlementResult::AlreadySettled(format!(
                "result={}",
                existing.0
            )));
        }

        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // ── 1. Load HCR ──────────────────────────────────────────────────
        let hcr: Option<(String, String)> = tx
            .query_row(
                "SELECT status, harness_id FROM harness_change_requests WHERE request_id = ?1",
                params![hcr_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (hcr_status, hcr_harness) =
            hcr.ok_or_else(|| anyhow::anyhow!("SETTLE_HCR_NOT_FOUND"))?;
        if hcr_status != "running" {
            tx.commit()?;
            return Ok(SettlementResult::AlreadySettled(format!(
                "status={}",
                hcr_status
            )));
        }

        // ── 2. Load claim ────────────────────────────────────────────────
        let claim: Option<(String,String)> = tx.query_row(
            "SELECT claim_id, harness_id FROM hcr_claims WHERE hcr_id = ?1 AND status = 'active'",
            params![hcr_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (db_claim_id, claim_harness) =
            claim.ok_or_else(|| anyhow::anyhow!("SETTLE_NO_CLAIM"))?;
        if db_claim_id != claim_id {
            anyhow::bail!("SETTLE_CLAIM_MISMATCH");
        }
        if claim_harness != hcr_harness {
            anyhow::bail!("SETTLE_HARNESS_MISMATCH");
        }

        // ── 3. Load Run binding ──────────────────────────────────────────
        let binding: Option<(String, String, String)> = tx
            .query_row(
                "SELECT run_id, hcr_id, binding_id FROM hcr_run_bindings WHERE claim_id = ?1",
                params![claim_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (db_run_id, bind_hcr, _) =
            binding.ok_or_else(|| anyhow::anyhow!("SETTLE_NO_BINDING"))?;
        if db_run_id != run_id {
            anyhow::bail!("SETTLE_RUN_MISMATCH");
        }
        if bind_hcr != hcr_id {
            anyhow::bail!("SETTLE_BIND_HCR_MISMATCH");
        }

        // ── 4. Load Run and verify RunMode::Hcr ──────────────────────────
        let run_mode_str: Option<String> = tx
            .query_row(
                "SELECT mode FROM runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;
        let mode_str = run_mode_str.ok_or_else(|| anyhow::anyhow!("SETTLE_RUN_NOT_FOUND"))?;
        let mode: RunMode = serde_json::from_str(&mode_str).unwrap_or(RunMode::Default);
        match &mode {
            RunMode::Hcr {
                hcr_id: rh,
                claim_id: rc,
                harness_id: _,
            } => {
                if rh != hcr_id {
                    anyhow::bail!("SETTLE_RUNMODE_HCR_MISMATCH");
                }
                if rc != claim_id {
                    anyhow::bail!("SETTLE_RUNMODE_CLAIM_MISMATCH");
                }
            }
            _ => anyhow::bail!("SETTLE_RUN_NOT_HCR"),
        }

        // ── 5. Load the five gate attempts ──────────────────────────────
        let attempt_rows: Vec<(String, String, String, String, String, String, String)>;
        let a_ids: Vec<String>;
        {
            let mut stmt = tx.prepare(
                "SELECT gate_attempt_id, gate_kind, expected_operation, expected_profile,
                        workspace_id, harness_id, invocation_intent_id
                 FROM hcr_gate_attempts
                 WHERE hcr_id = ?1 AND claim_id = ?2 AND run_id = ?3
                 ORDER BY gate_kind",
            )?;
            let rows: Vec<_> = stmt
                .query_map(params![hcr_id, claim_id, run_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            attempt_rows = rows;
        }

        if attempt_rows.len() != 5 {
            tx.commit()?;
            return Ok(SettlementResult::EvidenceIncomplete(format!(
                "attempts {}",
                attempt_rows.len()
            )));
        }

        a_ids = attempt_rows.iter().map(|a| a.0.clone()).collect();
        let a_kinds: Vec<String> = attempt_rows.iter().map(|a| a.1.clone()).collect();
        let expected: Vec<&str> = GateKind::all_required()
            .iter()
            .map(|k| k.as_str())
            .collect();
        let mut sorted_kinds = a_kinds.clone();
        sorted_kinds.sort();
        let mut sorted_expected: Vec<&str> = expected.to_vec();
        sorted_expected.sort();
        if sorted_kinds != sorted_expected {
            tx.commit()?;
            return Ok(SettlementResult::EvidenceIncomplete(format!(
                "kinds {:?} vs {:?}",
                sorted_kinds, sorted_expected
            )));
        }

        // ── 6. Load evidence for each attempt ────────────────────────────
        let ev_rows: Vec<(String, String, String, String)>;
        {
            let placeholders: Vec<String> = (1..=a_ids.len()).map(|i| format!("?{}", i)).collect();
            let ev_sql = format!(
                "SELECT gate_attempt_id, evidence_id, receipt_event_id, receipt_payload_digest
                 FROM hcr_gate_evidence WHERE gate_attempt_id IN ({}) ORDER BY gate_attempt_id",
                placeholders.join(","),
            );
            let mut ev_stmt = tx.prepare(&ev_sql)?;
            let ev_params: Vec<Box<dyn rusqlite::types::ToSql>> = a_ids
                .iter()
                .map(|s| Box::new(s.clone()) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            let ev_refs: Vec<&dyn rusqlite::types::ToSql> =
                ev_params.iter().map(|b| b.as_ref()).collect();
            let rows: Vec<_> = ev_stmt
                .query_map(ev_refs.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            ev_rows = rows;
        }

        if ev_rows.len() != 5 {
            tx.commit()?;
            return Ok(SettlementResult::EvidenceIncomplete(format!(
                "evidence {}",
                ev_rows.len()
            )));
        }

        // ── 7. For each evidence, load and validate receipt/intent events ─
        let mut infra = false;
        let mut candidate = false;
        let mut first_err = String::new();
        let mut parsed: Vec<ValidatedGateReceipt> = Vec::new();

        for (aid, akind, aop, _aprof, _aws, _ahar, aintent) in attempt_rows.iter() {
            // Find matching evidence by gate_attempt_id.
            let ev_pair = match ev_rows.iter().find(|e| &e.0 == aid) {
                Some(e) => e,
                None => {
                    infra = true;
                    first_err = format!("no evidence for {aid}");
                    continue;
                }
            };
            let ev_receipt_id = &ev_pair.2;
            let ev_digest = &ev_pair.3;

            // Load intent event (matched by correlation_id, which is set to intent_id).
            let intent_ev: Option<(String,String)> = tx.query_row(
                "SELECT kind, payload_json FROM journal_events WHERE correlation_id = ?1 AND kind = 'InvocationProposed'",
                params![aintent],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let (_, intent_payload_str) =
                intent_ev.ok_or_else(|| anyhow::anyhow!("VALIDATE_INTENT_NOT_FOUND"))?;
            let intent_payload: serde_json::Value = serde_json::from_str(&intent_payload_str)?;
            let intent_op = intent_payload
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if intent_op != aop {
                candidate = true;
                first_err = format!("{} op mismatch: expected {aop}", akind);
                continue;
            }

            // Load receipt event.
            let receipt_ev: Option<(String, String)> = tx
                .query_row(
                    "SELECT kind, payload_json FROM journal_events WHERE event_id = ?1",
                    params![ev_receipt_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (receipt_kind, receipt_payload_str) =
                receipt_ev.ok_or_else(|| anyhow::anyhow!("EVIDENCE_RECEIPT_NOT_FOUND"))?;
            if receipt_kind != "ReceiptReceived" {
                infra = true;
                first_err = format!("{} event kind {}", akind, receipt_kind);
                continue;
            }
            let receipt_payload: serde_json::Value = serde_json::from_str(&receipt_payload_str)?;

            // Validate payload digest (must match validate.rs computation).
            let mut hasher = sha2::Sha256::new();
            hasher.update(format!("{:?}", receipt_payload).as_bytes());
            let calc_digest = format!("sha256:{}", hex::encode(hasher.finalize()));
            if &calc_digest != ev_digest {
                infra = true;
                first_err = format!("{} digest mismatch", akind);
                continue;
            }

            let out = receipt_payload.get("output");
            let status = receipt_payload
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            let exit_code = out
                .and_then(|o| o.get("exit_code"))
                .and_then(|v| v.as_i64())
                .unwrap_or(-1) as i32;
            let timed_out = out
                .and_then(|o| o.get("timed_out"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let child_cleanup = out
                .and_then(|o| o.get("child_cleanup"))
                .and_then(|v| v.as_bool());
            let err_cat = out
                .and_then(|o| o.get("error_category"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Classification.
            if timed_out {
                infra = true;
                first_err = format!("{} timed out", akind);
                continue;
            }
            if child_cleanup != Some(true) {
                infra = true;
                first_err = format!("{} cleanup={:?}", akind, child_cleanup);
                continue;
            }
            if err_cat.is_some() {
                infra = true;
                first_err = format!("{} error={:?}", akind, err_cat);
                continue;
            }
            if status == "Failed" || exit_code != 0 {
                candidate = true;
                first_err = format!("{} failed st={} exit={}", akind, status, exit_code);
                continue;
            }

            parsed.push(ValidatedGateReceipt {
                gate_attempt_id: aid.clone(),
                receipt_event_id: ev_receipt_id.clone(),
                status: status.to_string(),
                exit_code,
                timed_out,
                child_cleanup,
                error_code: err_cat,
                receipt_payload_digest: calc_digest,
                operation: aop.to_string(),
            });
        }

        if infra {
            tx.commit()?;
            return Ok(SettlementResult::InfrastructureFailure(first_err));
        }
        if parsed.len() != 5 {
            tx.commit()?;
            return Ok(SettlementResult::EvidenceIncomplete(format!(
                "{} of 5 valid",
                parsed.len()
            )));
        }

        // ── 8. Compute digest and result ────────────────────────────────
        let has_failure = parsed
            .iter()
            .any(|r| r.exit_code != 0 || r.status == "Failed");
        let (settlement_result, hcr_new_status) = if has_failure {
            ("candidate_failed", "failed")
        } else {
            ("succeeded", "succeeded")
        };

        let mut digest_hasher = sha2::Sha256::new();
        digest_hasher.update(hcr_id.as_bytes());
        digest_hasher.update(b"|");
        digest_hasher.update(claim_id.as_bytes());
        digest_hasher.update(b"|");
        digest_hasher.update(run_id.as_bytes());
        digest_hasher.update(b"|");
        for r in &parsed {
            digest_hasher.update(r.gate_attempt_id.as_bytes());
            digest_hasher.update(b"|");
            digest_hasher.update(r.receipt_event_id.as_bytes());
            digest_hasher.update(b"|");
            digest_hasher.update(r.receipt_payload_digest.as_bytes());
            digest_hasher.update(b"|");
            digest_hasher.update(r.status.as_bytes());
            digest_hasher.update(b"|");
            digest_hasher.update(&r.exit_code.to_le_bytes());
            digest_hasher.update(b"|");
        }
        let digest = format!("sha256:{}", hex::encode(digest_hasher.finalize()));

        // ── 9. CAS update HCR ───────────────────────────────────────────
        let error_code = if has_failure {
            Some(first_err.as_str())
        } else {
            None
        };
        let now = chrono::Utc::now().to_rfc3339();
        let updated = tx.execute(
            "UPDATE harness_change_requests SET status = ?1, error_code = ?2, updated_at = ?3
             WHERE request_id = ?4 AND status = 'running'",
            params![hcr_new_status, error_code, now, hcr_id],
        )?;
        if updated == 0 {
            tx.commit()?;
            return Ok(SettlementResult::AlreadySettled(
                "CAS skipped, already terminal".into(),
            ));
        }

        // ── 10. Insert settlement record ─────────────────────────────────
        let settlement_id = format!("stl_{}", uuid::Uuid::new_v4().simple());
        tx.execute(
            "INSERT INTO hcr_settlements (settlement_id, hcr_id, claim_id, run_id, result, error_code, evidence_set_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![settlement_id, hcr_id, claim_id, run_id, settlement_result, error_code, digest, now],
        )?;

        // ── 11. Insert terminal journal event ────────────────────────────
        let terminal_kind = if settlement_result == "succeeded" {
            JournalEventKind::HcrSettlementSucceeded
        } else {
            JournalEventKind::HcrSettlementFailed
        };
        let ev_id = EventId::new();
        let kind_text = format!("{:?}", terminal_kind);
        let payload_json = serde_json::to_string(&json!({
            "hcr_id": hcr_id, "claim_id": claim_id, "run_id": run_id,
            "result": settlement_result, "error_code": error_code,
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

        // ── 12. COMMIT ──────────────────────────────────────────────────
        tx.commit()?;

        if has_failure {
            Ok(SettlementResult::CandidateFailed(settlement_id))
        } else {
            Ok(SettlementResult::Succeeded(settlement_id))
        }
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
