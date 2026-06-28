//! Test-only helpers exposed on `JournalStore` so integration tests can drive
//! projection state without touching private connection fields.
//!
//! This entire module is compiled out unless `cfg(test)` or the `test-helpers`
//! Cargo feature is enabled (HANDOVER §4.1). Production builds (`cargo build`)
//! never enable the feature, so these symbols are absent from release
//! artifacts. `cargo test` enables it via the self dev-dependency in
//! `Cargo.toml`.

use super::sqlite::JournalStore;
use crate::domain::{InvocationId, OutboxDispatchStatus, Run, RunId};
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::params;
use rusqlite::OptionalExtension;
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

    /// Overwrite the `kind` column of sequence 1 with an arbitrary string,
    /// simulating tampering or future-enum drift. Used to exercise the
    /// `parse_kind` fallback routing to `JournalEventKind::Unknown`.
    pub fn tamper_first_event_kind_for_test(&self, kind: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE journal_events SET kind = ?1 WHERE sequence = 1",
            params![kind],
        )?;
        Ok(())
    }

    /// Expire the lease of a `running` worker job (set `locked_until` to the
    /// past), simulating a worker loop crash mid-job. Used to exercise
    /// `worker_job_stale_count`.
    pub fn expire_worker_lease_for_test(&self, job_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let past = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        conn.execute(
            "UPDATE worker_jobs SET locked_until = ?1 WHERE job_id = ?2",
            params![past, job_id],
        )?;
        Ok(())
    }

    /// Force `PRAGMA user_version` to a specific value, simulating a database
    /// written by a newer kernel. Used to exercise the startup migration check
    /// (Phase 1 hardening).
    pub fn set_user_version_for_test(&self, version: i64) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.pragma_update(None, "user_version", version)?;
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

    /// Test-only deterministic fault injection for the `session.recall_recent`
    /// capability. When `fail` is true, only the capability-bound recall query
    /// returns a deterministic `Err`, while every other Journal operation (event append,
    /// run status update, `fail_run`, hash-chain verification) keeps working.
    /// This lets the capability-failure test exercise the real Runtime
    /// production chain — a real Failed Receipt is written to a still-writable
    /// Journal — instead of dropping a table (which also breaks receipt
    /// writing) or faking the error with `unwrap_or`. Compiled out of
    /// production builds.
    pub fn set_recall_failure_for_test(&self, fail: bool) {
        self.recall_failure_for_test
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    /// Simulate an operator acknowledging a terminal-unknown row (see
    /// `docs/decisions/ack-clear-terminal-unknown.md`, option 1). Mirrors the
    /// external ack SQL documented in the operating guide
    /// (`UPDATE outbox_dispatches SET acked_unknown=1 WHERE invocation_id=?`).
    /// Setting `ack=false` reverses it.
    pub fn ack_outbox_unknown_for_test(
        &self,
        invocation_id: &InvocationId,
        ack: bool,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE outbox_dispatches SET acked_unknown = ?1 WHERE invocation_id = ?2",
            params![if ack { 1 } else { 0 }, invocation_id.0],
        )?;
        Ok(())
    }

    /// Total number of entries in the `runs` table.
    pub fn run_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Number of runs currently in `Running` status.
    pub fn running_run_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE status = 'Running'",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Total number of registry snapshots.
    pub fn registry_snapshot_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row("SELECT COUNT(*) FROM registry_snapshots", [], |row| {
            row.get(0)
        })
        .map_err(Into::into)
    }

    /// Set the cached current snapshot ID without creating or verifying the
    /// snapshot. Used to test dangling-snapshot scenarios.
    pub fn set_current_snapshot_id_for_test(&self, snapshot_id: &str) {
        *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.to_string());
    }

    /// Create an in-memory journal with the CURRENT snapshot cleared after
    /// creation, so the registry is effectively uninitialized. Used to test
    /// deliver failure when no current snapshot exists.
    pub fn in_memory_without_registry() -> Result<Self> {
        let store = Self::in_memory()?;
        // Clear the cached snapshot ID — this simulates an uninitialized
        // registry without needing access to private with_conn/migrate.
        *store.current_snapshot_id.lock().unwrap() = None;
        Ok(store)
    }

    /// Look up a Run by ID.
    pub fn run(&self, run_id: &RunId) -> Result<Option<Run>> {
        use crate::domain::*;
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row: Option<(String, String, String, String, String, Option<String>, Option<String>, String, String, String, Option<String>)> = conn
            .query_row(
                "SELECT id, session_id, agent_id, trigger_event_id, principal_json, parent_run_id, delegated_by, status, created_at, updated_at, registry_snapshot_id
                 FROM runs WHERE id = ?1",
                params![run_id.0],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            session_id,
            agent_id,
            trigger_event_id,
            principal_json,
            parent_run_id,
            delegated_by,
            status,
            created_at,
            updated_at,
            registry_snapshot_id,
        )) = row
        else {
            return Ok(None);
        };
        let id: String = id;
        let session_id: String = session_id;
        let agent_id: String = agent_id;
        let trigger_event_id: String = trigger_event_id;
        let status: String = status;
        let created_at: String = created_at;
        let updated_at: String = updated_at;
        let parent_run_id: Option<String> = parent_run_id;
        let delegated_by: Option<String> = delegated_by;
        let registry_snapshot_id: Option<String> = registry_snapshot_id;
        let principal: RunPrincipal = serde_json::from_str(&principal_json)?;
        let run_status = match status.as_str() {
            "Running" => RunStatus::Running,
            "WaitingDispatch" => RunStatus::WaitingDispatch,
            "Completed" => RunStatus::Completed,
            "Failed" => RunStatus::Failed,
            "AwaitingApproval" => RunStatus::AwaitingApproval,
            _ => RunStatus::Unknown,
        };
        Ok(Some(Run {
            id: RunId(id),
            session_id: SessionId(session_id),
            agent_id: AgentId(agent_id),
            trigger_event_id: EventId(trigger_event_id),
            principal,
            parent_run_id: parent_run_id.map(RunId),
            delegated_by: delegated_by.map(PrincipalId),
            status: run_status,
            created_at: chrono::DateTime::parse_from_rfc3339(&created_at)?
                .with_timezone(&chrono::Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)?
                .with_timezone(&chrono::Utc),
            registry_snapshot_id: registry_snapshot_id.unwrap_or_default(),
        }))
    }
}
