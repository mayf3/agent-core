use super::sqlite::JournalStore;
use anyhow::{anyhow, Result};
use std::collections::BTreeMap;

impl JournalStore {
    pub fn worker_job_status_counts(&self) -> Result<BTreeMap<String, i64>> {
        self.status_counts("worker_jobs")
    }

    pub fn outbox_dispatch_status_counts(&self) -> Result<BTreeMap<String, i64>> {
        self.status_counts("outbox_dispatches")
    }

    fn status_counts(&self, table: &str) -> Result<BTreeMap<String, i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT status, COUNT(*) FROM {table} GROUP BY status ORDER BY status"
        ))?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
        rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
            .map_err(Into::into)
    }
}
