//! Test-only helpers exposed on `JournalStore` so integration tests can drive
//! projection state without touching private connection fields. Production
//! code must never call any of these.

use super::sqlite::JournalStore;
use crate::domain::{InvocationId, OutboxDispatchStatus};
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::params;
use serde_json::json;

impl JournalStore {
    pub fn tamper_first_event_for_test(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE journal_events SET payload_json = ?1 WHERE sequence = 1",
            params![json!({"tampered": true}).to_string()],
        )?;
        Ok(())
    }

    /// Expire an outbox lease so recovery queries select the row.
    pub fn expire_outbox_lease_for_test(&self, invocation_id: &InvocationId) -> Result<()> {
        let past = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE outbox_dispatches SET locked_until = ?1 WHERE invocation_id = ?2",
            params![past, invocation_id.0],
        )?;
        Ok(())
    }

    /// Mark a `retryable_failed` outbox row as immediately re-leasable by
    /// moving `available_at` to the past.
    pub fn set_outbox_available_at_past_for_test(
        &self,
        invocation_id: &InvocationId,
    ) -> Result<()> {
        let past = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE outbox_dispatches SET available_at = ?1 WHERE invocation_id = ?2",
            params![past, invocation_id.0],
        )?;
        Ok(())
    }

    /// Force-set the projection status of an outbox row. Used by tests that
    /// need to assert lease behavior against each non-pending state.
    pub fn set_outbox_status_for_test(
        &self,
        invocation_id: &InvocationId,
        status: OutboxDispatchStatus,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE outbox_dispatches SET status = ?1 WHERE invocation_id = ?2",
            params![status.as_str(), invocation_id.0],
        )?;
        Ok(())
    }
}
