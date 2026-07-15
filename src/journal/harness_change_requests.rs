//! Durable HarnessChangeRequest persistence.
//!
//! PR4A1: create pending requests. PR4A2: consume and transition them.
//! The `harness_change_requests` table is the source of truth.
//! The `HarnessChangeRequested` journal event is a same-transaction audit record.

use super::hash_chain::event_hash;
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::params;
use rusqlite::OptionalExtension;
use serde_json::Value;

/// Append a journal event inside an existing transaction. Used by
/// `create_harness_change_request` to write both the domain row and the
/// journal event atomically.
fn append_event_in_tx(
    tx: &rusqlite::Transaction,
    kind: JournalEventKind,
    run_id: Option<&RunId>,
    session_id: Option<&SessionId>,
    correlation_id: Option<&str>,
    payload: Value,
) -> Result<JournalEvent> {
    let event_id = EventId::new();
    let created_at = Utc::now();
    let payload_json = serde_json::to_string(&payload)?;
    let kind_text = kind.storage_name();
    let previous = tx
        .query_row(
            "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let sequence = previous.as_ref().map(|(seq, _)| seq + 1).unwrap_or(1);
    let previous_hash = previous.map(|(_, hash)| hash);
    let hash = event_hash(
        previous_hash.as_deref(),
        sequence,
        &kind_text,
        &payload_json,
    );
    tx.execute(
        "INSERT INTO journal_events
         (sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            sequence,
            event_id.0,
            run_id.map(|id| id.0.as_str()),
            session_id.map(|id| id.0.as_str()),
            correlation_id,
            kind_text,
            payload_json,
            previous_hash,
            hash,
            created_at.to_rfc3339(),
        ],
    )?;
    Ok(JournalEvent {
        sequence,
        event_id,
        run_id: run_id.cloned(),
        session_id: session_id.cloned(),
        correlation_id: correlation_id.map(str::to_string),
        kind,
        payload,
        previous_hash,
        hash,
        created_at,
    })
}

impl JournalStore {
    /// Create a pending HarnessChangeRequest and append a `HarnessChangeRequested`
    /// event in the same SQLite transaction.
    ///
    /// Returns the new request_id. If a request with the same (source, source_message_id)
    /// already exists, returns the existing request_id with `deduplicated = true`.
    pub fn create_harness_change_request(
        &self,
        source: &str,
        source_message_id: &str,
        session_id: &str,
        principal_id: &str,
        channel: &str,
        chat_type: &str,
        harness_id: &str,
        requirement: &str,
    ) -> Result<(String, bool)> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        // Check for existing request with the same dedup key.
        let existing: Option<String> = conn
            .query_row(
                "SELECT request_id FROM harness_change_requests
                 WHERE source = ?1 AND source_message_id = ?2",
                params![source, source_message_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(request_id) = existing {
            return Ok((request_id, true));
        }

        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // Re-check after acquiring the database write lock. Another
        // JournalStore connection may have won between the optimistic lookup
        // above and BEGIN IMMEDIATE.
        if let Some(request_id) = tx
            .query_row(
                "SELECT request_id FROM harness_change_requests
                 WHERE source = ?1 AND source_message_id = ?2",
                params![source, source_message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            tx.commit()?;
            return Ok((request_id, true));
        }

        let request_id = format!("hcr_{}", uuid::Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();

        tx.execute(
            "INSERT INTO harness_change_requests
             (request_id, source, source_message_id, session_id, principal_id,
              channel, chat_type, harness_id, requirement, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?10)",
            params![
                request_id,
                source,
                source_message_id,
                session_id,
                principal_id,
                channel,
                chat_type,
                harness_id,
                requirement,
                now,
            ],
        )?;

        // Append a journal event as same-transaction audit record.
        append_event_in_tx(
            &tx,
            JournalEventKind::HarnessChangeRequested,
            None,
            None,
            Some(&request_id),
            serde_json::json!({
                "request_id": request_id,
                "source": source,
                "source_message_id": source_message_id,
                "harness_id": harness_id,
                "requirement": requirement,
                "principal_id": principal_id,
                "channel": channel,
                "chat_type": chat_type,
                "session_id": session_id,
                "status": "pending",
            }),
        )?;

        tx.commit()?;
        Ok((request_id, false))
    }

    /// Look up a HarnessChangeRequest by request_id.
    pub fn get_harness_change_request(
        &self,
        request_id: &str,
    ) -> Result<Option<HarnessChangeRequest>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row = conn
            .query_row(
                "SELECT request_id, source, source_message_id, session_id, principal_id,
                        channel, chat_type, harness_id, requirement, status,
                        created_at, updated_at, run_id, error_code
                 FROM harness_change_requests WHERE request_id = ?1",
                params![request_id],
                |row| {
                    Ok(HarnessChangeRequest {
                        request_id: row.get(0)?,
                        source: row.get(1)?,
                        source_message_id: row.get(2)?,
                        session_id: row.get(3)?,
                        principal_id: row.get(4)?,
                        channel: row.get(5)?,
                        chat_type: row.get(6)?,
                        harness_id: row.get(7)?,
                        requirement: row.get(8)?,
                        status: row.get(9)?,
                        created_at: row.get(10)?,
                        updated_at: row.get(11)?,
                        run_id: row.get(12)?,
                        error_code: row.get(13)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Count of HarnessChangeRequest records (for test assertions).
    pub fn harness_change_request_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row("SELECT COUNT(*) FROM harness_change_requests", [], |row| {
            row.get(0)
        })
        .map_err(Into::into)
    }

    // ── Atomic claim ──────────────────────────────────────────────────────

    /// Atomically claim an HCR for execution. This is the first stateful action:
    ///
    /// 1. Loads the HCR and verifies it is in `pending` status.
    /// 2. Verifies no active claim already exists.
    /// 3. Transitions the HCR to `running` status.
    /// 4. Creates a claim record with `claim_id`, `worker_instance_id`, and timestamp.
    /// 5. Appends an `HcrClaimSucceeded` journal event.
    ///
    /// All steps happen in one SQLite transaction. On success, returns the
    /// new `ClaimId`. On failure (already claimed, wrong status), returns an
    /// error with a stable message prefix for the caller to match against.
    ///
    /// # Concurrency
    ///
    /// Uses `UPDATE ... WHERE status = 'pending'` + `changes()` to detect
    /// concurrent claims. At most one concurrent worker succeeds.
    /// The UNIQUE(hcr_id) constraint on `hcr_claims` is a second safety layer.
    pub fn claim_hcr_for_execution(
        &self,
        hcr_id: &str,
        harness_id: &str,
        worker_instance_id: &str,
    ) -> Result<ClaimId> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // 1. Load the HCR.
        let hcr: Option<HarnessChangeRequest> = tx
            .query_row(
                "SELECT request_id, source, source_message_id, session_id, principal_id,
                        channel, chat_type, harness_id, requirement, status,
                        created_at, updated_at, run_id, error_code
                 FROM harness_change_requests WHERE request_id = ?1",
                params![hcr_id],
                |row| {
                    Ok(HarnessChangeRequest {
                        request_id: row.get(0)?,
                        source: row.get(1)?,
                        source_message_id: row.get(2)?,
                        session_id: row.get(3)?,
                        principal_id: row.get(4)?,
                        channel: row.get(5)?,
                        chat_type: row.get(6)?,
                        harness_id: row.get(7)?,
                        requirement: row.get(8)?,
                        status: row.get(9)?,
                        created_at: row.get(10)?,
                        updated_at: row.get(11)?,
                        run_id: row.get(12)?,
                        error_code: row.get(13)?,
                    })
                },
            )
            .optional()?;

        let hcr = hcr.ok_or_else(|| anyhow!("HCR_NOT_FOUND: {hcr_id}"))?;

        // 2. Verify the HCR is claimable (pending status).
        if hcr.status != "pending" {
            bail!(
                "HCR_NOT_CLAIMABLE: status is {}, expected pending",
                hcr.status
            );
        }

        // 3. Atomic compare-and-swap: transition to running.
        // Only succeeds if still pending (handles concurrent workers).
        let now = Utc::now().to_rfc3339();
        let updated = tx.execute(
            "UPDATE harness_change_requests
             SET status = 'running', updated_at = ?1
             WHERE request_id = ?2 AND status = 'pending'",
            params![now, hcr_id],
        )?;
        if updated == 0 {
            // Another worker already claimed it.
            bail!("HCR_ALREADY_CLAIMED: {hcr_id}");
        }

        // 4. Check no active claim already exists (defense in depth).
        let existing_claim: Option<String> = tx
            .query_row(
                "SELECT claim_id FROM hcr_claims WHERE hcr_id = ?1 AND status = 'active'",
                params![hcr_id],
                |row| row.get(0),
            )
            .optional()?;
        if existing_claim.is_some() {
            bail!("HCR_ALREADY_CLAIMED: active claim exists for {hcr_id}");
        }

        // 5. Generate claim_id and insert claim record.
        let claim_id = ClaimId::new();
        tx.execute(
            "INSERT INTO hcr_claims (claim_id, hcr_id, harness_id, worker_instance_id, claimed_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active')",
            params![claim_id.0, hcr_id, harness_id, worker_instance_id, now],
        )?;

        // 6. Append audit event.
        append_event_in_tx(
            &tx,
            JournalEventKind::HcrClaimSucceeded,
            None,
            None,
            Some(&claim_id.0),
            serde_json::json!({
                "claim_id": claim_id.0,
                "hcr_id": hcr_id,
                "harness_id": harness_id,
                "worker_instance_id": worker_instance_id,
                "claimed_at": now,
            }),
        )?;

        tx.commit()?;
        Ok(claim_id)
    }

    /// Look up the active claim for an HCR, if any.
    pub fn get_active_claim_for_hcr(&self, hcr_id: &str) -> Result<Option<HcrClaim>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row = conn
            .query_row(
                "SELECT claim_id, hcr_id, harness_id, worker_instance_id, claimed_at, status
                 FROM hcr_claims WHERE hcr_id = ?1 AND status = 'active'",
                params![hcr_id],
                |row| {
                    let status_str: String = row.get(5)?;
                    Ok(HcrClaim {
                        claim_id: ClaimId(row.get(0)?),
                        hcr_id: row.get(1)?,
                        harness_id: row.get(2)?,
                        worker_instance_id: row.get(3)?,
                        claimed_at: row.get(4)?,
                        status: HcrClaimStatus::parse_opt(&status_str)
                            .unwrap_or(HcrClaimStatus::Released),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Look up a claim by claim_id.
    pub fn get_claim(&self, claim_id: &str) -> Result<Option<HcrClaim>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row = conn
            .query_row(
                "SELECT claim_id, hcr_id, harness_id, worker_instance_id, claimed_at, status
                 FROM hcr_claims WHERE claim_id = ?1",
                params![claim_id],
                |row| {
                    let status_str: String = row.get(5)?;
                    Ok(HcrClaim {
                        claim_id: ClaimId(row.get(0)?),
                        hcr_id: row.get(1)?,
                        harness_id: row.get(2)?,
                        worker_instance_id: row.get(3)?,
                        claimed_at: row.get(4)?,
                        status: HcrClaimStatus::parse_opt(&status_str)
                            .unwrap_or(HcrClaimStatus::Released),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    // ── Run binding ───────────────────────────────────────────────────────

    /// Create (or return existing) binding between an HCR claim and a Run.
    ///
    /// Idempotent: if a binding for `(hcr_id, claim_id)` already exists,
    /// returns the existing `(run_id, is_resume=true)`. Otherwise inserts a
    /// new binding and returns `(run_id, is_resume=false)`.
    ///
    /// The UNIQUE(hcr_id, claim_id) constraint prevents duplicate bindings.
    pub fn create_hcr_run_binding(
        &self,
        hcr_id: &str,
        claim_id: &str,
        run_id: &str,
    ) -> Result<(String, bool)> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        // Check for existing binding.
        let existing: Option<String> = conn
            .query_row(
                "SELECT run_id FROM hcr_run_bindings
                 WHERE hcr_id = ?1 AND claim_id = ?2",
                params![hcr_id, claim_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(existing_run_id) = existing {
            return Ok((existing_run_id, true));
        }

        let binding_id = format!("bind_{}", uuid::Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT OR IGNORE INTO hcr_run_bindings
             (binding_id, hcr_id, claim_id, run_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![binding_id, hcr_id, claim_id, run_id, now],
        )?;

        Ok((run_id.to_string(), false))
    }

    /// Look up the run binding for a claim, if any.
    pub fn get_run_binding_for_claim(&self, claim_id: &str) -> Result<Option<HcrRunBinding>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row = conn
            .query_row(
                "SELECT binding_id, hcr_id, claim_id, run_id, created_at
                 FROM hcr_run_bindings WHERE claim_id = ?1",
                params![claim_id],
                |row| {
                    Ok(HcrRunBinding {
                        binding_id: row.get(0)?,
                        hcr_id: row.get(1)?,
                        claim_id: row.get(2)?,
                        run_id: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Look up the run binding for a run_id, if any.
    pub fn get_hcr_binding_for_run(&self, run_id: &str) -> Result<Option<HcrRunBinding>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row = conn
            .query_row(
                "SELECT binding_id, hcr_id, claim_id, run_id, created_at
                 FROM hcr_run_bindings WHERE run_id = ?1",
                params![run_id],
                |row| {
                    Ok(HcrRunBinding {
                        binding_id: row.get(0)?,
                        hcr_id: row.get(1)?,
                        claim_id: row.get(2)?,
                        run_id: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Count of claim records (for test assertions).
    pub fn hcr_claim_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row("SELECT COUNT(*) FROM hcr_claims", [], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Count of run binding records (for test assertions).
    pub fn hcr_run_binding_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row("SELECT COUNT(*) FROM hcr_run_bindings", [], |row| {
            row.get(0)
        })
        .map_err(Into::into)
    }
}
