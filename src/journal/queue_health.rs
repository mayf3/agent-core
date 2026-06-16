use super::sqlite::JournalStore;
use crate::domain::OutboxDispatchStatus;
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
