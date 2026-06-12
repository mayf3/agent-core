use super::queue::append_event_tx;
use super::sqlite::JournalStore;
use crate::domain::{EventId, JournalEventKind};
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::json;

impl JournalStore {
    pub fn lease_next_worker_job(&self) -> Result<Option<EventId>> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row = tx
            .query_row(
                "SELECT job_id, source_event_id
                 FROM worker_jobs
                 WHERE status = 'queued' AND available_at <= ?1
                 ORDER BY available_at, created_at
                 LIMIT 1",
                params![Utc::now().to_rfc3339()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((job_id, source_event_id)) = row else {
            tx.commit()?;
            return Ok(None);
        };
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE worker_jobs
             SET status = 'running', attempts = attempts + 1, updated_at = ?1
             WHERE job_id = ?2 AND status = 'queued'",
            params![now.as_str(), job_id.as_str()],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(None);
        }
        append_event_tx(
            &tx,
            JournalEventKind::WorkerJobStarted,
            None,
            None,
            Some(&job_id),
            json!({
                "job_id": job_id,
                "job_type": "deliver_event",
                "source_event_id": source_event_id,
                "status": "running",
                "attempted_at": now,
            }),
        )?;
        tx.commit()?;
        Ok(Some(EventId(source_event_id)))
    }
}
