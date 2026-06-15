use super::queue::{append_event_tx, append_worker_event_tx, worker_job_id};
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
use chrono::{Duration, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

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
                     status = ?2
                     OR (status = ?3 AND available_at <= ?1)
                     OR (status = ?4 AND (locked_until IS NULL OR locked_until <= ?1))
                   )
                 ORDER BY available_at, created_at
                 LIMIT 1",
                params![
                    now_text.as_str(),
                    WorkerJobStatus::Queued.as_str(),
                    WorkerJobStatus::RetryableFailed.as_str(),
                    WorkerJobStatus::Running.as_str(),
                ],
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
             SET status = ?4,
                  attempts = attempts + 1,
                  locked_by = 'kernel-worker',
                  locked_until = ?1,
                  updated_at = ?2
             WHERE job_id = ?3
               AND (
                 status = ?5
                 OR (status = ?6 AND available_at <= ?2)
                 OR (status = ?7 AND (locked_until IS NULL OR locked_until <= ?2))
               )",
            params![
                locked_until.as_str(),
                now_text.as_str(),
                job_id.as_str(),
                WorkerJobStatus::Running.as_str(),
                WorkerJobStatus::Queued.as_str(),
                WorkerJobStatus::RetryableFailed.as_str(),
                WorkerJobStatus::Running.as_str(),
            ],
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
                "status": WorkerJobStatus::Running.as_str(),
                "attempted_at": now_text,
                "locked_until": locked_until,
            }),
        )?;
        tx.commit()?;
        Ok(Some(EventId(source_event_id)))
    }

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
             VALUES (?1, 'deliver_event', ?2, ?3, 0, ?4, ?4, ?4)",
             params![
                 job_id.as_str(),
                 event.event_id.0.as_str(),
                 WorkerJobStatus::Queued.as_str(),
                 now.as_str(),
             ],
         )?;
         if changed == 1 {
             append_worker_event_tx(
                 &tx,
                 JournalEventKind::WorkerJobQueued,
                 &job_id,
                 &event.event_id,
                 json!({ "status": WorkerJobStatus::Queued.as_str() }),
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
             VALUES (?1, 'deliver_event', ?2, ?3, 0, ?4, ?4, ?4)",
            params![
                job_id.as_str(),
                source_event_id.0.as_str(),
                WorkerJobStatus::Queued.as_str(),
                now.as_str(),
            ],
        )?;
        if changed == 1 {
            append_worker_event_tx(
                &tx,
                JournalEventKind::WorkerJobQueued,
                &job_id,
                source_event_id,
                json!({ "status": WorkerJobStatus::Queued.as_str() }),
            )?;
        }
        tx.commit()?;
        Ok(job_id)
    }

    pub fn worker_job_status(&self, job_id: &str) -> Result<Option<WorkerJobStatus>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM worker_jobs WHERE job_id = ?1",
                params![job_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(status.and_then(|s| WorkerJobStatus::from_str(&s)))
    }

    pub fn start_worker_job(&self, source_event_id: &EventId) -> Result<()> {
        self.update_worker_job(
            source_event_id,
            WorkerJobStatus::Running,
            JournalEventKind::WorkerJobStarted,
            json!({ "status": WorkerJobStatus::Running.as_str() }),
        )
    }

    pub fn succeed_worker_job(&self, source_event_id: &EventId) -> Result<()> {
        self.update_worker_job(
            source_event_id,
            WorkerJobStatus::Succeeded,
            JournalEventKind::WorkerJobSucceeded,
            json!({ "status": WorkerJobStatus::Succeeded.as_str() }),
        )
    }

    pub fn fail_worker_job(&self, source_event_id: &EventId, error_category: &str) -> Result<()> {
        self.update_worker_job(
            source_event_id,
            WorkerJobStatus::Failed,
            JournalEventKind::WorkerJobFailed,
            json!({ "status": WorkerJobStatus::Failed.as_str(), "error_category": error_category }),
        )
    }

    pub fn mark_worker_retryable_failed(
        &self,
        source_event_id: &EventId,
        error: &str,
        policy: &RetryPolicy,
    ) -> Result<()> {
        let job_id = worker_job_id(source_event_id);
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let attempts: i64 = tx.query_row(
            "SELECT attempts FROM worker_jobs WHERE job_id = ?1",
            params![job_id],
            |row| row.get(0),
        ).optional()?.unwrap_or(0);
        if attempts >= policy.max_worker_attempts {
            drop(tx);
            drop(conn);
            return self.mark_worker_dead(source_event_id, error, policy);
        }
        let delay_ms = next_retry_delay_ms(attempts + 1, policy.base_retry_delay_ms, policy.max_retry_delay_ms);
        let available_at = now + chrono::Duration::milliseconds(delay_ms);
        let available_at_text = available_at.to_rfc3339();
        append_worker_event_tx(
            &tx,
            JournalEventKind::WorkerJobFailed,
            &job_id,
            source_event_id,
            json!({
                "status": WorkerJobStatus::RetryableFailed.as_str(),
                "error": error,
                "retryable": true,
                "next_available_at": available_at_text,
                "attempts": attempts,
            }),
        )?;
        tx.execute(
            "UPDATE worker_jobs SET status = ?1, last_error = ?2,
             locked_by = NULL, locked_until = NULL,
             available_at = ?3, updated_at = ?4
             WHERE job_id = ?5",
            params![
                WorkerJobStatus::RetryableFailed.as_str(),
                error,
                available_at_text,
                now_text,
                job_id,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_worker_dead(
        &self,
        source_event_id: &EventId,
        error: &str,
        _policy: &RetryPolicy,
    ) -> Result<()> {
        let job_id = worker_job_id(source_event_id);
        let now_text = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        append_worker_event_tx(
            &tx,
            JournalEventKind::WorkerJobDead,
            &job_id,
            source_event_id,
            json!({
                "status": WorkerJobStatus::Dead.as_str(),
                "error": error,
            }),
        )?;
        tx.execute(
            "UPDATE worker_jobs SET status = ?1, last_error = ?2,
             locked_by = NULL, locked_until = NULL, updated_at = ?3
             WHERE job_id = ?4",
            params![
                WorkerJobStatus::Dead.as_str(),
                error,
                now_text,
                job_id,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn update_worker_job(
        &self,
        source_event_id: &EventId,
        status: WorkerJobStatus,
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
        let status_str = status.as_str();
        let changed = if status == WorkerJobStatus::Running {
            tx.execute(
                "UPDATE worker_jobs
                 SET status = ?1, attempts = attempts + 1, updated_at = ?2
                 WHERE job_id = ?3 AND status != ?4",
                params![status_str, now, job_id.as_str(), WorkerJobStatus::Succeeded.as_str()],
            )?
        } else {
            tx.execute(
                "UPDATE worker_jobs
                 SET status = ?1,
                     last_error = ?2,
                     locked_by = NULL,
                     locked_until = NULL,
                     updated_at = ?3
                 WHERE job_id = ?4 AND status != ?5",
                params![
                    status_str,
                    payload
                        .get("error_category")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    now,
                    job_id.as_str(),
                    WorkerJobStatus::Succeeded.as_str(),
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

#[cfg(test)]
mod tests {
    use super::*;
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

        assert_eq!(
            journal.worker_job_status(&job_id)?.as_ref(),
            Some(&WorkerJobStatus::Running)
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
