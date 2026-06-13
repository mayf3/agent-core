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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use anyhow::Result;
    use rusqlite::params;

    #[test]
    fn lease_reclaims_stale_running_job() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let source_event_id = EventId("event_stale".to_string());
        let job_id = journal.enqueue_worker_job(&source_event_id)?;

        // First lease makes it running with locked_until = now + 5min.
        assert_eq!(
            journal.lease_next_worker_job()?,
            Some(source_event_id.clone())
        );

        // Manually expire the lease to simulate a worker crash.
        let past = (Utc::now() - Duration::hours(1)).to_rfc3339();
        {
            let conn = journal.conn.lock().unwrap();
            conn.execute(
                "UPDATE worker_jobs SET locked_until = ?1 WHERE job_id = ?2",
                params![past, job_id],
            )?;
        }

        // Second lease should reclaim the stale running job.
        assert_eq!(journal.lease_next_worker_job()?, Some(source_event_id));

        // Status stays 'running'.
        assert_eq!(
            journal.worker_job_status(&job_id)?.as_deref(),
            Some("running")
        );

        // A new WorkerJobStarted event was appended by the second lease.
        let started_events = journal
            .events()?
            .into_iter()
            .filter(|e| e.kind == JournalEventKind::WorkerJobStarted)
            .count();
        assert_eq!(started_events, 2);

        assert!(journal.verify_hash_chain()?);
        Ok(())
    }

    #[test]
    fn lease_does_not_reclaim_active_running_job() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let source_event_id = EventId("event_active".to_string());
        let job_id = journal.enqueue_worker_job(&source_event_id)?;

        // First lease makes it running.
        assert_eq!(
            journal.lease_next_worker_job()?,
            Some(source_event_id.clone())
        );

        // Extend locked_until far into the future to simulate a busy worker.
        let far_future = (Utc::now() + Duration::days(30)).to_rfc3339();
        {
            let conn = journal.conn.lock().unwrap();
            conn.execute(
                "UPDATE worker_jobs SET locked_until = ?1 WHERE job_id = ?2",
                params![far_future, job_id],
            )?;
        }

        // Second lease should not reclaim while locked_until is still in the future.
        assert_eq!(journal.lease_next_worker_job()?, None);

        // Still only one WorkerJobStarted event.
        let started_events = journal
            .events()?
            .into_iter()
            .filter(|e| e.kind == JournalEventKind::WorkerJobStarted)
            .count();
        assert_eq!(started_events, 1);

        assert!(journal.verify_hash_chain()?);
        Ok(())
    }
}
