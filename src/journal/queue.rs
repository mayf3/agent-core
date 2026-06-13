use super::hash_chain::event_hash;
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
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
    Ok(())
}

impl JournalStore {
    pub fn accept_ingress_with_worker_job(
        &self,
        event: &ValidatedEvent,
        payload: Value,
    ) -> Result<String> {
        let job_id = worker_job_id(&event.event_id);
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        append_event_tx(
            &tx,
            JournalEventKind::IngressAccepted,
            None,
            None,
            Some(&event.dedupe_key),
            payload,
        )?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO worker_jobs
             (job_id, job_type, source_event_id, status, attempts, available_at, created_at, updated_at)
             VALUES (?1, 'deliver_event', ?2, 'queued', 0, ?3, ?3, ?3)",
            params![job_id.as_str(), event.event_id.0.as_str(), now.as_str()],
        )?;
        if changed == 1 {
            append_worker_event_tx(
                &tx,
                JournalEventKind::WorkerJobQueued,
                &job_id,
                &event.event_id,
                json!({ "status": "queued" }),
            )?;
        }
        tx.commit()?;
        Ok(job_id)
    }

    pub fn enqueue_worker_job(&self, source_event_id: &EventId) -> Result<String> {
        let job_id = worker_job_id(source_event_id);
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO worker_jobs
             (job_id, job_type, source_event_id, status, attempts, available_at, created_at, updated_at)
             VALUES (?1, 'deliver_event', ?2, 'queued', 0, ?3, ?3, ?3)",
            params![job_id.as_str(), source_event_id.0.as_str(), now.as_str()],
        )?;
        if changed == 1 {
            append_worker_event_tx(
                &tx,
                JournalEventKind::WorkerJobQueued,
                &job_id,
                source_event_id,
                json!({ "status": "queued" }),
            )?;
        }
        tx.commit()?;
        Ok(job_id)
    }

    pub fn worker_job_status(&self, job_id: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT status FROM worker_jobs WHERE job_id = ?1",
            params![job_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn start_worker_job(&self, source_event_id: &EventId) -> Result<()> {
        self.update_worker_job(
            source_event_id,
            "running",
            JournalEventKind::WorkerJobStarted,
            json!({ "status": "running" }),
        )
    }

    pub fn succeed_worker_job(&self, source_event_id: &EventId) -> Result<()> {
        self.update_worker_job(
            source_event_id,
            "succeeded",
            JournalEventKind::WorkerJobSucceeded,
            json!({ "status": "succeeded" }),
        )
    }

    pub fn fail_worker_job(&self, source_event_id: &EventId, error_category: &str) -> Result<()> {
        self.update_worker_job(
            source_event_id,
            "failed",
            JournalEventKind::WorkerJobFailed,
            json!({ "status": "failed", "error_category": error_category }),
        )
    }

    pub fn queue_outbox_dispatch(
        &self,
        approved: &ApprovedInvocation,
        session_id: Option<&SessionId>,
    ) -> Result<String> {
        let intent = approved.intent();
        let dispatch_id = format!("dispatch:{}", intent.invocation_id.0);
        let idempotency_key = intent
            .idempotency_key
            .clone()
            .unwrap_or_else(|| intent.invocation_id.0.clone());
        let arguments_json = serde_json::to_string(&intent.arguments)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO outbox_dispatches
             (dispatch_id, invocation_id, run_id, session_id, operation, arguments_json,
              idempotency_key, status, attempts, available_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', 0, ?8, ?8, ?8)",
            params![
                dispatch_id.as_str(),
                intent.invocation_id.0.as_str(),
                intent.run_id.0.as_str(),
                session_id.map(|id| id.0.as_str()),
                intent.operation.as_str(),
                arguments_json.as_str(),
                idempotency_key.as_str(),
                now.as_str(),
            ],
        )?;
        if changed == 1 {
            append_event_tx(
                &tx,
                JournalEventKind::OutboxQueued,
                Some(&intent.run_id),
                session_id,
                Some(&intent.invocation_id.0),
                json!({
                    "dispatch_id": dispatch_id,
                    "invocation_id": intent.invocation_id.0.as_str(),
                    "operation": intent.operation.as_str(),
                    "idempotency_key": idempotency_key,
                    "status": "pending",
                }),
            )?;
        }
        tx.commit()?;
        Ok(dispatch_id)
    }

    pub fn outbox_dispatch_status(&self, invocation_id: &InvocationId) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT status FROM outbox_dispatches WHERE invocation_id = ?1",
            params![invocation_id.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn start_outbox_dispatch(
        &self,
        approved: &ApprovedInvocation,
        session_id: Option<&SessionId>,
    ) -> Result<()> {
        let intent = approved.intent();
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE outbox_dispatches
             SET status = 'dispatching', attempts = attempts + 1, updated_at = ?1
             WHERE invocation_id = ?2 AND status = 'pending'",
            params![now.as_str(), intent.invocation_id.0.as_str()],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Err(anyhow!("outbox_dispatch_not_startable"));
        }
        append_event_tx(
            &tx,
            JournalEventKind::DispatchStarted,
            Some(&intent.run_id),
            session_id,
            Some(&intent.invocation_id.0),
            json!({ "operation": intent.operation.as_str() }),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn succeed_outbox_dispatch(
        &self,
        receipt: &Receipt,
        run_id: &RunId,
        session_id: Option<&SessionId>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE outbox_dispatches
             SET status = 'succeeded', updated_at = ?1
             WHERE invocation_id = ?2",
            params![now.as_str(), receipt.invocation_id.0.as_str()],
        )?;
        append_event_tx(
            &tx,
            JournalEventKind::ReceiptReceived,
            Some(run_id),
            session_id,
            Some(&receipt.invocation_id.0),
            json!({
                "status": format!("{:?}", receipt.status),
                "external_ref": receipt.external_ref,
                "output_kind": "text",
            }),
        )?;
        tx.commit()?;
        Ok(())
    }

    fn update_worker_job(
        &self,
        source_event_id: &EventId,
        status: &str,
        event_kind: JournalEventKind,
        mut payload: Value,
    ) -> Result<()> {
        let job_id = worker_job_id(source_event_id);
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = if status == "running" {
            tx.execute(
                "UPDATE worker_jobs
                 SET status = ?1, attempts = attempts + 1, updated_at = ?2
                 WHERE job_id = ?3 AND status != 'succeeded'",
                params![status, now, job_id.as_str()],
            )?
        } else {
            tx.execute(
                "UPDATE worker_jobs
                 SET status = ?1,
                     last_error = ?2,
                     locked_by = NULL,
                     locked_until = NULL,
                     updated_at = ?3
                 WHERE job_id = ?4 AND status != 'succeeded'",
                params![
                    status,
                    payload
                        .get("error_category")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    now,
                    job_id.as_str(),
                ],
            )?
        };
        if changed > 0 {
            payload["attempted_at"] = json!(now);
            append_worker_event_tx(&tx, event_kind, &job_id, source_event_id, payload)?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn worker_job_id(source_event_id: &EventId) -> String {
    format!("job:deliver:{}", source_event_id.0)
}

fn append_worker_event_tx(
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
    append_event_tx(&tx, kind, None, None, Some(job_id), payload)
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
