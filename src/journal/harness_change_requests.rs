//! Durable HarnessChangeRequest persistence.
//!
//! PR4A1: create pending requests. PR4A2: consume and transition them.
//! The `harness_change_requests` table is the source of truth.
//! The `HarnessChangeRequested` journal event is a same-transaction audit record.

use super::hash_chain::event_hash;
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
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
    let kind_text = format!("{:?}", kind);
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
}
