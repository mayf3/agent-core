//! Test-only helpers exposed on `JournalStore` so integration tests can drive
//! projection state without touching private connection fields.
//!
//! This entire module is compiled out unless `cfg(test)` or the `test-helpers`
//! Cargo feature is enabled (HANDOVER §4.1). Production builds (`cargo build`)
//! never enable the feature, so these symbols are absent from release
//! artifacts. `cargo test` enables it via the self dev-dependency in
//! `Cargo.toml`.

use super::sqlite::JournalStore;
use crate::domain::{InvocationId, OutboxDispatchStatus, Run, RunId, Session};
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::params;
use serde_json::json;

impl JournalStore {
    /// Counts retained HCR workflow rows so new-flow tests can prove that the
    /// production path does not create any active-workflow facts.
    pub fn hcr_fact_counts_for_test(&self) -> Result<(i64, i64, i64, i64)> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        Ok((
            conn.query_row("SELECT COUNT(*) FROM harness_change_requests", [], |row| {
                row.get(0)
            })?,
            conn.query_row("SELECT COUNT(*) FROM hcr_claims", [], |row| row.get(0))?,
            conn.query_row("SELECT COUNT(*) FROM hcr_gate_attempts", [], |row| {
                row.get(0)
            })?,
            conn.query_row("SELECT COUNT(*) FROM hcr_settlements", [], |row| row.get(0))?,
        ))
    }

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

    /// Get the cached current snapshot ID. Used to verify cache refresh after
    /// CAS conflict.
    pub fn get_current_snapshot_id_for_test(&self) -> Option<String> {
        self.current_snapshot_id.lock().unwrap().clone()
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
        self.run_by_id(run_id)
    }

    /// Execute a raw SQL batch (e.g. CREATE TRIGGER) for test-only fault
    /// injection. Not available in production builds.
    pub fn execute_sql_for_test(&self, sql: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute_batch(sql)?;
        Ok(())
    }
}

impl JournalStore {
    /// Test-support: persist an existing snapshot (header + operations +
    /// hook bindings) so the narrow invocation entry can reload it by id.
    pub fn persist_snapshot_for_tests(&self, snapshot: &RegistrySnapshot) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("journal mutex poisoned"))?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT snapshot_id FROM registry_snapshots WHERE snapshot_id = ?1",
                params![snapshot.snapshot_id],
                |row| row.get(0),
            )
            .ok();
        if exists.is_some() {
            return Ok(());
        }
        conn.execute(
            "INSERT INTO registry_snapshots (snapshot_id, created_at, operation_count, canonical_digest)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                snapshot.snapshot_id,
                snapshot.created_at.to_rfc3339(),
                snapshot.operations.len() as i64,
                snapshot.snapshot_id,
            ],
        )?;
        let mut sorted = snapshot.operations.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for op in &sorted {
            conn.execute(
                "INSERT INTO registry_snapshot_operations
                 (snapshot_id, operation_name, risk, description, parameters_json, idempotent, binding_kind, binding_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    snapshot.snapshot_id,
                    op.name,
                    format!("{:?}", op.risk),
                    op.description,
                    serde_json::to_string(&op.parameters)?,
                    op.idempotent as i64,
                    format!("{:?}", op.binding_kind),
                    op.binding_key,
                ],
            )?;
        }
        for b in &snapshot.hook_bindings {
            conn.execute(
                "INSERT INTO registry_snapshot_hook_bindings
                 (snapshot_id, contract, hook_id, hook_version, binding_kind, binding_key, provider_id, endpoint)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    snapshot.snapshot_id,
                    b.contract,
                    b.hook_id,
                    b.hook_version,
                    format!("{:?}", b.binding_kind),
                    b.binding_key,
                    b.provider_id,
                    b.endpoint,
                ],
            )?;
        }
        Ok(())
    }

    /// Test-support: insert a session row exactly as built (used by
    /// fixtures that construct sessions with fixed ids).
    pub fn insert_session_for_tests(&self, session: &Session) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "INSERT INTO sessions
             (id, agent_id, channel, conversation_key, summary, summarized_until_event_id, last_active_at, status, version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                session.id.0,
                session.agent_id.0,
                format!("{:?}", session.channel),
                session.conversation_key,
                session.summary,
                session.summarized_until_event_id.as_ref().map(|e| e.0.clone()),
                session.last_active_at.to_rfc3339(),
                format!("{:?}", session.status),
                session.version,
            ],
        )?;
        Ok(())
    }
}
