use super::hash_chain::event_hash;
use crate::domain::*;
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE UNIQUE INDEX IF NOT EXISTS idx_runs_trigger_event
        ON runs(trigger_event_id);

        CREATE TABLE IF NOT EXISTS worker_jobs (
          job_id TEXT PRIMARY KEY,
          job_type TEXT NOT NULL,
          source_event_id TEXT NOT NULL,
          session_id TEXT,
          run_id TEXT,
          status TEXT NOT NULL,
          attempts INTEGER NOT NULL DEFAULT 0,
          available_at TEXT NOT NULL,
          locked_by TEXT,
          locked_until TEXT,
          last_error TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_worker_jobs_ready
        ON worker_jobs(status, available_at);

        CREATE INDEX IF NOT EXISTS idx_worker_jobs_source_event
        ON worker_jobs(source_event_id);

        CREATE TABLE IF NOT EXISTS outbox_dispatches (
          dispatch_id TEXT PRIMARY KEY,
          invocation_id TEXT NOT NULL UNIQUE,
          run_id TEXT NOT NULL,
          session_id TEXT,
          operation TEXT NOT NULL,
          arguments_json TEXT NOT NULL,
          idempotency_key TEXT NOT NULL,
          decision_id TEXT NOT NULL DEFAULT '',
          acked_unknown INTEGER NOT NULL DEFAULT 0,
          status TEXT NOT NULL,
          attempts INTEGER NOT NULL DEFAULT 0,
          available_at TEXT NOT NULL,
          locked_by TEXT,
          locked_until TEXT,
          last_error TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_outbox_ready
        ON outbox_dispatches(status, available_at);

        CREATE INDEX IF NOT EXISTS idx_outbox_run_id
        ON outbox_dispatches(run_id);
        ",
    )?;

    ensure_outbox_decision_id_column(conn)?;
    ensure_outbox_acked_unknown_column(conn)?;
    Ok(())
}

fn ensure_outbox_decision_id_column(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(outbox_dispatches)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "decision_id" {
            return Ok(());
        }
    }
    conn.execute_batch(
        "ALTER TABLE outbox_dispatches ADD COLUMN decision_id TEXT NOT NULL DEFAULT '';",
    )?;
    Ok(())
}

/// Add the `acked_unknown` column (see
/// `docs/decisions/ack-clear-terminal-unknown.md`, option 1). An operator sets
/// it to `1` via an external script to acknowledge that a terminal-unknown
/// row's lost outcome is known and should no longer degrade `/health.status`.
/// Idempotent: existing DBs (and the in-memory schema above) get the column
/// added with default `0`; the in-memory `CREATE TABLE` already declares it.
fn ensure_outbox_acked_unknown_column(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(outbox_dispatches)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "acked_unknown" {
            return Ok(());
        }
    }
    conn.execute_batch(
        "ALTER TABLE outbox_dispatches ADD COLUMN acked_unknown INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

pub(crate) fn worker_job_id(source_event_id: &EventId) -> String {
    format!("job:deliver:{}", source_event_id.0)
}

pub(crate) fn append_worker_event_tx(
    tx: &Transaction<'_>,
    kind: JournalEventKind,
    job_id: &str,
    source_event_id: &EventId,
    extra_payload: Value,
) -> Result<JournalEvent> {
    let mut payload = json!({
        "job_id": job_id,
        "job_type": "deliver_event",
        "source_event_id": source_event_id.0.as_str(),
    });
    if let (Some(base), Some(extra)) = (payload.as_object_mut(), extra_payload.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    append_event_tx(tx, kind, None, None, Some(job_id), payload)
}

pub(crate) fn append_event_tx(
    tx: &Transaction<'_>,
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
