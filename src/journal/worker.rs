use super::queue::append_event_tx;
use super::sqlite::JournalStore;
use crate::domain::{EventId, JournalEventKind};
use anyhow::{anyhow, Result};
use chrono::{Duration, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::json;

impl JournalStore {
    pub fn lease_next_worker_job(&self) -> Result<Option<EventId>> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let row = tx
            .query_row(
                "SELECT job_id, source_event_id
                 FROM worker_jobs
                 WHERE available_at <= ?1
                   AND (
                     status = 'queued'
                     OR (status = 'running' AND (locked_until IS NULL OR locked_until <= ?1))
                   )
                 ORDER BY available_at, created_at
                 LIMIT 1",
                params![now_text.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((job_id, source_event_id)) = row else {
            tx.commit()?;
            return Ok(None);
        };
        let locked_until = (now + Duration::minutes(5)).to_rfc3339();
        let changed = tx.execute(
            "UPDATE worker_jobs
             SET status = 'running',
                 attempts = attempts + 1,
                 locked_by = 'kernel-worker',
                 locked_until = ?1,
                 updated_at = ?2
             WHERE job_id = ?3
               AND (
                 status = 'queued'
                 OR (status = 'running' AND (locked_until IS NULL OR locked_until <= ?2))
               )",
            params![locked_until.as_str(), now_text.as_str(), job_id.as_str()],
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
                "attempted_at": now_text,
                "locked_until": locked_until,
            }),
        )?;
        tx.commit()?;
        Ok(Some(EventId(source_event_id)))
    }
}
