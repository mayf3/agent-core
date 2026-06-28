//! Registry snapshot operations on JournalStore. These methods use the same
//! SQLite connection as the rest of the Journal, sharing its mutex. The
//! registry tables (migration 0002) store immutable snapshots that each Run
//! pins to for its lifetime.
//!
//! The current snapshot is durable in `registry_current_state` (migration 0003)
//! so that activation and rollback survive restart.

use crate::registry::snapshot::{
    compute_snapshot_id, BindingKind, OperationSpec, RegistrySnapshot, Risk,
};
use crate::registry::store::builtin_specs;
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use std::sync::Arc;

impl super::JournalStore {
    /// Initialize the registry at Kernel boot: read durable current snapshot
    /// from DB if one exists; otherwise create baseline, persist it as current,
    /// and backfill old Runs. This is the **only** path that sets the current
    /// snapshot ID at startup.
    ///
    /// **Persistence contract**: the `registry_current_state` singleton row is
    /// the source of truth. If it exists its `snapshot_id` must be loadable and
    /// strictly decodable — otherwise boot fails (never silently fall back to
    /// baseline).
    pub fn initialize_registry(&self) -> Result<String> {
        // 1. Try to read the durable current snapshot from DB.
        if let Some(snapshot_id) = self.read_persistent_current()? {
            // Verify the snapshot is loadable (triggers strict decode).
            let _ = self.load_registry_snapshot(&snapshot_id)?;
            *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.clone());
            let _ = self.backfill_null_registry_snapshot(&snapshot_id);
            return Ok(snapshot_id);
        }

        // 2. No durable current exists — create baseline, persist, backfill.
        let snapshot = self.create_registry_snapshot(builtin_specs())?;
        let snapshot_id = snapshot.snapshot_id.clone();
        self.write_persistent_current(&snapshot_id)?;
        *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.clone());
        let _ = self.backfill_null_registry_snapshot(&snapshot_id);
        Ok(snapshot_id)
    }

    /// Read the durable current snapshot_id from `registry_current_state`.
    /// Returns `None` if the table exists but is empty (fresh DB with migration
    /// 0003 applied but no row yet).
    fn read_persistent_current(&self) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        // Check if the table exists.
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='registry_current_state'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !table_exists {
            return Ok(None);
        }
        let id: Option<String> = conn
            .query_row(
                "SELECT snapshot_id FROM registry_current_state WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id)
    }

    /// Write (or update) the durable current snapshot_id.
    fn write_persistent_current(&self, snapshot_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO registry_current_state (singleton_id, snapshot_id, updated_at)
             VALUES (1, ?1, ?2)",
            params![snapshot_id, now],
        )?;
        Ok(())
    }

    /// Read-only getter for the currently active registry snapshot ID.
    /// Returns `registry_snapshot_unavailable` if the registry has not been
    /// initialized (e.g. `initialize_registry()` was never called, or the
    /// cached ID was cleared). Never creates or switches snapshots.
    pub fn current_registry_snapshot_id(&self) -> Result<String> {
        self.current_snapshot_id
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("registry_snapshot_unavailable: no current registry snapshot"))
    }

    /// Create (or return existing) an immutable snapshot from specs. If the same
    /// canonical digest already exists, the existing snapshot is returned.
    pub fn create_registry_snapshot(&self, specs: Vec<OperationSpec>) -> Result<RegistrySnapshot> {
        let snapshot_id = compute_snapshot_id(&specs)?;
        let created_at = chrono::Utc::now();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        // Check if snapshot already exists.
        let existing: Option<String> = conn
            .query_row(
                "SELECT snapshot_id FROM registry_snapshots WHERE snapshot_id = ?1",
                params![&snapshot_id],
                |row| row.get(0),
            )
            .ok();
        if existing.is_some() {
            return Self::load_snapshot_from_conn(&conn, &snapshot_id);
        }

        // Insert snapshot header.
        conn.execute(
            "INSERT INTO registry_snapshots (snapshot_id, created_at, operation_count, canonical_digest)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                &snapshot_id,
                created_at.to_rfc3339(),
                specs.len() as i64,
                &snapshot_id
            ],
        )?;

        // Insert operations (sorted by name for stable storage order).
        let mut sorted = specs;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for op in &sorted {
            conn.execute(
                "INSERT INTO registry_snapshot_operations
                 (snapshot_id, operation_name, risk, description, parameters_json, idempotent, binding_kind, binding_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &snapshot_id,
                    &op.name,
                    format!("{:?}", op.risk),
                    &op.description,
                    serde_json::to_string(&op.parameters)?,
                    op.idempotent as i64,
                    format!("{:?}", op.binding_kind),
                    &op.binding_key,
                ],
            )?;
        }
        drop(conn);
        Ok(RegistrySnapshot {
            snapshot_id,
            created_at,
            operations: sorted,
        })
    }

    /// Load a snapshot by ID. Returns an Arc for cheap Run-local cloning.
    pub fn load_registry_snapshot(&self, snapshot_id: &str) -> Result<Arc<RegistrySnapshot>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let snap = Self::load_snapshot_from_conn(&conn, snapshot_id)?;
        Ok(Arc::new(snap))
    }

    fn load_snapshot_from_conn(
        conn: &rusqlite::Connection,
        snapshot_id: &str,
    ) -> Result<RegistrySnapshot> {
        let created_at_str: String = conn.query_row(
            "SELECT created_at FROM registry_snapshots WHERE snapshot_id = ?1",
            params![snapshot_id],
            |row| row.get(0),
        )?;
        let created_at =
            chrono::DateTime::parse_from_rfc3339(&created_at_str)?.with_timezone(&chrono::Utc);

        let mut stmt = conn.prepare(
            "SELECT operation_name, risk, description, parameters_json, idempotent, binding_kind, binding_key
             FROM registry_snapshot_operations
             WHERE snapshot_id = ?1
             ORDER BY operation_name",
        )?;
        let operations: Vec<OperationSpec> = stmt
            .query_map(params![snapshot_id], |row| {
                let name: String = row.get(0)?;
                let risk_str: String = row.get(1)?;
                let description: String = row.get(2)?;
                let params_json: String = row.get(3)?;
                let idempotent: i64 = row.get(4)?;
                let binding_kind_str: String = row.get(5)?;
                let binding_key: String = row.get(6)?;
                let risk = match risk_str.as_str() {
                    "ReadOnly" => Risk::ReadOnly,
                    "Write" => Risk::Write,
                    other => {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "unknown risk string '{other}' in snapshot {snapshot_id}"
                        )))
                    }
                };
                let binding_kind = match binding_kind_str.as_str() {
                    "Builtin" => BindingKind::Builtin,
                    "ExternalHarness" => BindingKind::ExternalHarness,
                    other => {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "unknown binding_kind string '{other}' in snapshot {snapshot_id}"
                        )))
                    }
                };
                let parameters: serde_json::Value = match serde_json::from_str(&params_json) {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "invalid parameters JSON in snapshot {snapshot_id}: {e}"
                        )))
                    }
                };
                Ok(OperationSpec {
                    name,
                    risk,
                    description,
                    parameters,
                    idempotent: idempotent != 0,
                    binding_kind,
                    binding_key,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(RegistrySnapshot {
            snapshot_id: snapshot_id.to_string(),
            created_at,
            operations,
        })
    }

    /// Activate a snapshot as current (for new Runs). Persists the choice
    /// durably in `registry_current_state` so it survives restart.
    ///
    /// Transaction boundary: verify target exists → update persistent current
    /// → append RegistrySnapshotActivated event → commit. On failure the
    /// durable current and memory pointer are both unchanged.
    pub fn activate_registry_snapshot(&self, snapshot_id: &str) -> Result<()> {
        // Verify it exists (triggers strict decode).
        let _ = self.load_registry_snapshot(snapshot_id)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        // Update persistent current.
        tx.execute(
            "INSERT OR REPLACE INTO registry_current_state (singleton_id, snapshot_id, updated_at)
             VALUES (1, ?1, ?2)",
            params![snapshot_id, now],
        )?;
        // Append RegistrySnapshotActivated event.
        let payload = serde_json::json!({
            "snapshot_id": snapshot_id,
            "activated_at": now,
        });
        crate::journal::hash_chain::append_event_in_transaction(
            &tx,
            "RegistrySnapshotActivated",
            &serde_json::to_string(&payload)?,
            &now,
        )?;
        tx.commit()?;
        // On success, update the in-memory pointer.
        *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.to_string());
        Ok(())
    }

    /// Backfill old Runs with NULL registry_snapshot_id.
    fn backfill_null_registry_snapshot(&self, snapshot_id: &str) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let count = conn.execute(
            "UPDATE runs SET registry_snapshot_id = ?1 WHERE registry_snapshot_id IS NULL",
            params![snapshot_id],
        )?;
        Ok(count)
    }
}
