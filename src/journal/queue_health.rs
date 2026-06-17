use super::sqlite::JournalStore;
use crate::domain::{OutboxDispatchStatus, WorkerJobStatus};
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::params;
use std::collections::BTreeMap;

impl JournalStore {
    pub fn worker_job_status_counts(&self) -> Result<BTreeMap<String, i64>> {
        self.status_counts("worker_jobs")
    }

    pub fn outbox_dispatch_status_counts(&self) -> Result<BTreeMap<String, i64>> {
        self.status_counts("outbox_dispatches")
    }

    pub fn outbox_status_count(&self, status: OutboxDispatchStatus) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT COUNT(*) FROM outbox_dispatches WHERE status = ?1",
            params![status.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Count terminal-unknown outbox rows that have **not** been acknowledged
    /// by an operator. This is the count `/health.outbox_unknown_count` and
    /// the 档 C rollup use: an acknowledged row (`acked_unknown = 1`) is a
    /// terminal-unknown the operator has explicitly accepted, so it no longer
    /// degrades health. See `docs/decisions/ack-clear-terminal-unknown.md`.
    pub fn outbox_unknown_unacked_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT COUNT(*) FROM outbox_dispatches
             WHERE status = ?1 AND acked_unknown = 0",
            params![OutboxDispatchStatus::Unknown.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Count outbox rows still flagged `dispatching` whose lease has expired
    /// (`locked_until` is non-NULL and `<= now`). A row with a NULL
    /// `locked_until` is NOT counted as stale — it is owned by the dispatcher
    /// loop (queued via `start_outbox_dispatch`, which does not take an inline
    /// lease), not an inline-leased dispatch. Operators use this count to
    /// detect inline-leased dispatches that were abandoned mid-flight (e.g. a
    /// crash after `lease_next_outbox_dispatch`). Phase 1 Operational
    /// Hardening.
    pub fn outbox_stale_dispatching_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let now = Utc::now().to_rfc3339();
        conn.query_row(
            "SELECT COUNT(*) FROM outbox_dispatches
             WHERE status = ?1 AND locked_until IS NOT NULL AND locked_until <= ?2",
            params![OutboxDispatchStatus::Dispatching.as_str(), now.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Count outbox projection rows whose status disagrees with the Journal's
    /// terminal fact for the same invocation. A row is "drifted" when the
    /// Journal already has a terminal event (`ReceiptReceived` or
    /// `OutboxDispatchUnknown`) but the projection is not in the matching
    /// terminal state. At steady state this is 0 — startup recovery
    /// reconciles drift. A persistent non-zero count signals that recovery
    /// failed to run or a race left the projection inconsistent. Phase 1
    /// Operational Hardening (projection verify).
    pub fn outbox_projection_drift_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        // A row drifts if its Journal has a terminal fact but the projection
        // status is not the matching terminal state. We treat any projection
        // state that is NOT the Journal-implied terminal state as drift.
        conn.query_row(
            "SELECT COUNT(*) FROM outbox_dispatches od
             WHERE EXISTS (
               SELECT 1 FROM journal_events je
               WHERE je.correlation_id = od.invocation_id
                 AND je.kind IN ('ReceiptReceived', 'OutboxDispatchUnknown')
             )
             AND NOT (
               (od.status = 'succeeded' AND EXISTS (
                  SELECT 1 FROM journal_events je
                  WHERE je.correlation_id = od.invocation_id
                    AND je.kind = 'ReceiptReceived'
                    AND je.payload_json LIKE '%\"status\":\"Succeeded\"%'
               ))
               OR (od.status = 'failed' AND EXISTS (
                  SELECT 1 FROM journal_events je
                  WHERE je.correlation_id = od.invocation_id
                    AND je.kind = 'ReceiptReceived'
                    AND je.payload_json LIKE '%\"status\":\"Failed\"%'
               ))
               OR (od.status = 'unknown' AND EXISTS (
                  SELECT 1 FROM journal_events je
                  WHERE je.correlation_id = od.invocation_id
                    AND je.kind = 'OutboxDispatchUnknown'
               ))
             )",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Count worker jobs still flagged `running` whose lease has expired
    /// (`locked_until` is non-NULL and `<= now`). Symmetric to
    /// `outbox_stale_dispatching_count`: a non-zero count signals a worker
    /// loop that crashed mid-job (the lease was never released). Phase 1
    /// Operational Hardening.
    pub fn worker_job_stale_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let now = Utc::now().to_rfc3339();
        conn.query_row(
            "SELECT COUNT(*) FROM worker_jobs
             WHERE status = ?1 AND locked_until IS NOT NULL AND locked_until <= ?2",
            params![WorkerJobStatus::Running.as_str(), now.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    fn status_counts(&self, table: &str) -> Result<BTreeMap<String, i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT status, COUNT(*) FROM {table} GROUP BY status ORDER BY status"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
            .map_err(Into::into)
    }
}
