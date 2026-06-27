//! Registry snapshot operations on JournalStore. These methods use the same
//! SQLite connection as the rest of the Journal, sharing its mutex. The
//! registry tables (migration 0002) store immutable snapshots that each Run
//! pins to for its lifetime.

use crate::registry::snapshot::{
    compute_snapshot_id, BindingKind, OperationSpec, RegistrySnapshot, Risk,
};
use crate::registry::store::builtin_specs;
use anyhow::{anyhow, Result};
use rusqlite::params;
use std::sync::Arc;

impl super::JournalStore {
    /// Ensure the baseline registry snapshot exists and cache it as the current
    /// snapshot. Called at Kernel boot (after migrate). Also backfills old Runs
    /// with NULL registry_snapshot_id.
    pub fn ensure_baseline_registry(&self) -> Result<String> {
        let snapshot = self.create_registry_snapshot(builtin_specs())?;
        let snapshot_id = snapshot.snapshot_id.clone();
        *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.clone());
        // Backfill old Runs with NULL snapshot ID.
        let _ = self.backfill_null_registry_snapshot(&snapshot_id);
        Ok(snapshot_id)
    }

    /// The currently active snapshot ID (for new Run creation).
    /// Auto-creates the baseline snapshot if none exists (idempotent),
    /// so tests don't need an explicit ensure_baseline_registry() call.
    pub fn current_registry_snapshot_id(&self) -> Result<String> {
        if self.current_snapshot_id.lock().unwrap().is_none() {
            let _ = self.ensure_baseline_registry()?;
        }
        self.current_snapshot_id
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("no active registry snapshot"))
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
                    _ => Risk::Write,
                };
                let binding_kind = match binding_kind_str.as_str() {
                    "Builtin" => BindingKind::Builtin,
                    _ => BindingKind::Builtin,
                };
                let parameters: serde_json::Value =
                    serde_json::from_str(&params_json).unwrap_or(serde_json::json!({}));
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
            .filter_map(|r| r.ok())
            .collect();

        Ok(RegistrySnapshot {
            snapshot_id: snapshot_id.to_string(),
            created_at,
            operations,
        })
    }

    /// Activate a snapshot as current (for new Runs). Internal/test-only.
    pub fn activate_registry_snapshot(&self, snapshot_id: &str) -> Result<()> {
        // Verify it exists.
        let _ = self.load_registry_snapshot(snapshot_id)?;
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
