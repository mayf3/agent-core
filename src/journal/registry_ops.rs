//! Registry snapshot operations on JournalStore. These methods use the same
//! SQLite connection as the rest of the Journal, sharing its mutex. The
//! registry tables (migration 0002) store immutable snapshots that each Run
//! pins to for its lifetime.
use crate::registry::snapshot::{
    compute_snapshot_id, BindingKind, OperationSpec, RegistrySnapshot, Risk,
};
use crate::registry::store::builtin_specs;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::params;
use std::sync::Arc;
/// Narrow legacy constant to identify the retired builtin time.now operation.
/// This is only used in the upgrade migration path — it does NOT add time.now
/// to the catalog, provider tools, grants, or dispatch.
const LEGACY_BUILTIN_TIME_OPERATION: &str = "time.now";
const LEGACY_BUILTIN_TIME_BINDING: &str = "builtin.time_now";
impl super::JournalStore {
    /// Initialize the registry at Kernel boot: ensure the baseline snapshot
    /// exists, set it as current, and backfill old Runs. This is the **only**
    /// path that writes or sets the current snapshot ID — the runtime getter
    /// `current_registry_snapshot_id()` is a pure read. Called after `migrate()`
    /// during `JournalStore::open` or `serve` startup.
    ///
    /// Idempotent: if the baseline snapshot already exists (same canonical ID),
    /// it is reused; if `current_snapshot_id` is already set, it is preserved.
    ///
    /// If a legacy active snapshot contains the retired builtin time.now
    /// operation, it is automatically removed and a new snapshot is activated.
    /// The old snapshot remains immutable; the retired operation is never
    /// forwarded to an external harness, and no receipt is fabricated.
    pub fn initialize_registry(&self) -> Result<String> {
        // Check if we already have a cached current_snapshot_id (idempotent).
        if self.current_snapshot_id.lock().unwrap().is_some() {
            return self
                .current_snapshot_id
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow!("registry_initialized_but_id_missing"));
        }
        // Check if registry_state already exists (restart path).
        if let Some(state_snapshot_id) = self.load_active_snapshot_from_state()? {
            // Verify the snapshot exists in the DB.
            let snap = self.load_registry_snapshot(&state_snapshot_id)?;
            *self.current_snapshot_id.lock().unwrap() = Some(state_snapshot_id.clone());
            // Check for legacy builtin time.now to retire.
            match self.retire_legacy_builtin_time_if_present(&snap) {
                Ok(true) => {
                    // Retirement was performed; the cached ID is now updated
                    // by retire_legacy_builtin_time_if_present. No need to re-init.
                    // Return the NEW active snapshot ID.
                    return self
                        .current_snapshot_id
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or_else(|| anyhow!("registry_id_missing_after_retirement"));
                }
                Ok(false) => {
                    // No legacy time.now found — return the original.
                }
                Err(e) => {
                    // CAS conflict or other failure: refresh cache from DB
                    // so the cached ID matches the true active snapshot.
                    if let Ok(db_id) = self.load_active_snapshot_from_state() {
                        if let Some(ref db_sid) = db_id {
                            *self.current_snapshot_id.lock().unwrap() = Some(db_sid.clone());
                        }
                    }
                    return Err(e);
                }
            }
            return Ok(state_snapshot_id);
        }
        // First boot: create baseline snapshot, set as current, init state.
        let snapshot = self.create_registry_snapshot(builtin_specs())?;
        let snapshot_id = snapshot.snapshot_id.clone();
        *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.clone());
        self.init_registry_state(&snapshot_id)?;
        // Backfill old Runs with NULL snapshot ID.
        let _ = self.backfill_null_registry_snapshot(&snapshot_id);
        Ok(snapshot_id)
    }
    /// Check if the active snapshot contains the legacy builtin time.now
    /// operation. If so, create a new snapshot without it and atomically
    /// activate the new one via CAS + journal event. Returns true if
    /// retirement was performed.
    fn retire_legacy_builtin_time_if_present(
        &self,
        current_snap: &Arc<RegistrySnapshot>,
    ) -> Result<bool> {
        // Check if the active snapshot has legacy builtin time.now.
        let has_legacy = current_snap.operations.iter().any(|op| {
            op.name == LEGACY_BUILTIN_TIME_OPERATION
                && op.binding_kind == BindingKind::Builtin
                && op.binding_key == LEGACY_BUILTIN_TIME_BINDING
        });
        if !has_legacy {
            return Ok(false);
        }
        // Build a new spec list WITHOUT the legacy time.now.
        let new_specs: Vec<OperationSpec> = current_snap
            .operations
            .iter()
            .filter(|op| {
                !(op.name == LEGACY_BUILTIN_TIME_OPERATION
                    && op.binding_kind == BindingKind::Builtin
                    && op.binding_key == LEGACY_BUILTIN_TIME_BINDING)
            })
            .cloned()
            .collect();
        // Verify the new snapshot is different.
        if new_specs.len() == current_snap.operations.len() {
            // Should not happen since we checked has_legacy above, but guard.
            return Ok(false);
        }
        let new_snapshot = self.create_registry_snapshot(new_specs)?;
        let new_snapshot_id = new_snapshot.snapshot_id.clone();
        // Activate atomically with CAS + journal event.
        let old_id = &current_snap.snapshot_id;
        let decision_id = format!("retire_builtin_time:{}", old_id);
        self.apply_builtin_time_retirement(&new_snapshot_id, old_id, &decision_id)?;
        eprintln!(
            "retired legacy builtin time.now: {} -> {}",
            old_id, new_snapshot_id
        );
        Ok(true)
    }
    /// Atomically activate a retirement snapshot: CAS on registry_state,
    /// write RegistrySnapshotActivated journal event, update memory cache.
    ///
    /// `expected_snapshot_id` is the caller's expected active snapshot for CAS.
    /// On conflict, the in-memory cache is refreshed from DB before returning
    /// the error, so subsequent initialize_registry calls reflect truth.
    ///
    /// This is `pub(crate)` so tests can verify CAS failure with stale expected
    /// IDs without duplicating the CAS SQL.
    pub(crate) fn apply_builtin_time_retirement(
        &self,
        new_snapshot_id: &str,
        expected_snapshot_id: &str,
        decision_id: &str,
    ) -> Result<()> {
        use crate::domain::EventId;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| anyhow!("cannot begin transaction"))?;
        // CAS: read current registry_state and verify.
        let (db_snapshot_id, db_version): (String, i64) = tx
            .query_row(
                "SELECT active_snapshot_id, version FROM registry_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow!("registry_state not initialized"))?;
        if db_snapshot_id != expected_snapshot_id {
            // Refresh cache from DB before returning error.
            drop(tx);
            drop(conn);
            self.refresh_cache_from_db();
            bail!("snapshot_conflict: registry_state has {db_snapshot_id}, expected {expected_snapshot_id}");
        }
        let new_version = db_version + 1;
        let changed = tx.execute(
            "UPDATE registry_state SET active_snapshot_id = ?1, version = ?2, updated_at = ?3
             WHERE singleton_id = 1 AND version = ?4",
            params![
                new_snapshot_id,
                new_version,
                Utc::now().to_rfc3339(),
                db_version,
            ],
        )?;
        if changed == 0 {
            drop(tx);
            drop(conn);
            self.refresh_cache_from_db();
            bail!("snapshot_conflict: version CAS failed");
        }
        // Record journal event.
        let payload = serde_json::json!({
            "action": "retire_builtin_time",
            "previous_snapshot_id": expected_snapshot_id,
            "new_snapshot_id": new_snapshot_id,
            "decision_id": decision_id,
        });
        let kind_text = "RegistrySnapshotActivated";
        let event_id = EventId::new();
        let created_at = Utc::now();
        let payload_json = serde_json::to_string(&payload)?;
        let previous: Option<(i64, String)> = tx
            .query_row(
                "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        let sequence = previous.as_ref().map(|(seq, _)| seq + 1).unwrap_or(1);
        let previous_hash = previous.map(|(_, hash)| hash);
        let hash = crate::journal::hash_chain::event_hash(
            previous_hash.as_deref(),
            sequence,
            kind_text,
            &payload_json,
        );
        tx.execute(
            "INSERT INTO journal_events
             (sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                sequence, event_id.0,
                Option::<String>::None, Option::<String>::None,
                decision_id, kind_text, payload_json,
                previous_hash, hash, created_at.to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        drop(conn);
        *self.current_snapshot_id.lock().unwrap() = Some(new_snapshot_id.to_string());
        Ok(())
    }
    /// Refresh `current_snapshot_id` cache from DB's `registry_state`.
    fn refresh_cache_from_db(&self) {
        if let Ok(Some(db_id)) = self.load_active_snapshot_from_state() {
            *self.current_snapshot_id.lock().unwrap() = Some(db_id);
        }
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
                    _ => Risk::Write,
                };
                let binding_kind = match binding_kind_str.as_str() {
                    "Builtin" => BindingKind::Builtin,
                    "External" => BindingKind::External,
                    other => {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unknown binding_kind: {other}"),
                            )),
                        ));
                    }
                };
                let parameters: serde_json::Value =
                    serde_json::from_str(&params_json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("invalid parameters_json: {e}"),
                            )),
                        )
                    })?;
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
