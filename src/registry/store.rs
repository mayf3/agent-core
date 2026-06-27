use crate::registry::snapshot::{
    compute_snapshot_id, BindingKind, OperationSpec, RegistrySnapshot, Risk,
};
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

/// The Registry owns the baseline operation definitions and persists snapshots
/// to the Journal DB. It is passed by reference (not global/static/thread_local).
/// An in-memory cache (`current_snapshot`) avoids re-reading the DB on every
/// `load_snapshot`, but the DB is the source of truth on restart.
pub struct Registry {
    conn: Mutex<Connection>,
    /// Cached current snapshot (the latest activated). New Runs are created
    /// against this snapshot's ID. Held as Arc so Run-local clones are cheap.
    current_snapshot: Mutex<Option<Arc<RegistrySnapshot>>>,
}

/// The built-in baseline operation specs. These create the initial snapshot at
/// first boot and are the fallback for backfilling old Runs. Must produce a
/// deterministic snapshot ID.
pub fn builtin_specs() -> Vec<OperationSpec> {
    use serde_json::json;
    vec![
        OperationSpec {
            name: "stdout.send_text".into(),
            risk: Risk::Write,
            description: "send a text reply to the user (stdout).".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.stdout_send_text".into(),
        },
        OperationSpec {
            name: "feishu.send_message".into(),
            risk: Risk::Write,
            description: "send a message reply to the Feishu chat.".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.feishu_send_message".into(),
        },
        OperationSpec {
            name: "time.now".into(),
            risk: Risk::ReadOnly,
            description: "Return the current kernel wall-clock time (ISO-8601 + epoch ms).".into(),
            parameters: json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.time_now".into(),
        },
        OperationSpec {
            name: "session.recall_recent".into(),
            risk: Risk::ReadOnly,
            description:
                "Recall recent messages from the current session (read-only, current session only)."
                    .into(),
            parameters: json!({"type": "object", "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 20, "description": "Max messages to recall (default 5)."}, "query": {"type": "string", "description": "Optional case-insensitive substring filter."}}, "required": [], "additionalProperties": false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.session_recall_recent".into(),
        },
        OperationSpec {
            name: "system.status".into(),
            risk: Risk::ReadOnly,
            description:
                "Return system health and projection summary (aggregate counts only, no secrets)."
                    .into(),
            parameters: json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.system_status".into(),
        },
    ]
}

impl Registry {
    /// Open the registry using the same SQLite connection as the Journal. The
    /// migration (0002) must have been applied before this is called.
    pub fn new(conn: Connection) -> Result<Self> {
        let reg = Self {
            conn: Mutex::new(conn),
            current_snapshot: Mutex::new(None),
        };
        reg.ensure_baseline_snapshot()?;
        Ok(reg)
    }

    /// Ensure the baseline snapshot exists in the DB and cache it as current.
    /// This is called at Kernel boot: it creates the baseline snapshot if
    /// missing, and sets it as the active snapshot.
    pub fn ensure_baseline_snapshot(&self) -> Result<()> {
        let specs = builtin_specs();
        let snapshot = self.create_snapshot(specs)?;
        *self.current_snapshot.lock().unwrap() = Some(Arc::new(snapshot));
        Ok(())
    }

    /// Create (or return existing) a snapshot from specs. If a snapshot with the
    /// same canonical digest already exists, the existing one is returned —
    /// snapshots are append-only/immutable. Attempting to write a *different*
    /// content with the same snapshot_id fails.
    pub fn create_snapshot(&self, specs: Vec<OperationSpec>) -> Result<RegistrySnapshot> {
        let snapshot_id = compute_snapshot_id(&specs)?;
        let created_at = chrono::Utc::now();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("registry mutex poisoned"))?;

        // Check if this snapshot already exists.
        let existing: Option<String> = conn
            .query_row(
                "SELECT canonical_digest FROM registry_snapshots WHERE snapshot_id = ?1",
                params![&snapshot_id],
                |row| row.get(0),
            )
            .ok();
        if existing.is_some() {
            // Already persisted — load and return it.
            return self.load_snapshot_inner(&conn, &snapshot_id);
        }

        // Insert the snapshot header.
        let operation_count = specs.len() as i64;
        let canonical_digest = &snapshot_id; // The ID IS the digest.
        conn.execute(
            "INSERT INTO registry_snapshots (snapshot_id, created_at, operation_count, canonical_digest)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                &snapshot_id,
                created_at.to_rfc3339(),
                operation_count,
                canonical_digest
            ],
        )?;

        // Insert operation rows (sorted by name for stable storage order).
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

    /// The currently active snapshot ID (used for new Run creation).
    pub fn current_snapshot_id(&self) -> Result<String> {
        let guard = self.current_snapshot.lock().unwrap();
        guard
            .as_ref()
            .map(|s| s.snapshot_id.clone())
            .ok_or_else(|| anyhow!("no active snapshot — ensure_baseline_snapshot not called"))
    }

    /// Load a snapshot by ID from the DB. Used to recover a Run's exact
    /// operation set after restart. Returns a cheap Arc clone if cached.
    pub fn load_snapshot(&self, snapshot_id: &str) -> Result<Arc<RegistrySnapshot>> {
        // Check cache first.
        {
            let guard = self.current_snapshot.lock().unwrap();
            if let Some(ref snap) = *guard {
                if snap.snapshot_id == snapshot_id {
                    return Ok(Arc::clone(snap));
                }
            }
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("registry mutex poisoned"))?;
        let snap = self.load_snapshot_inner(&conn, snapshot_id)?;
        Ok(Arc::new(snap))
    }

    fn load_snapshot_inner(
        &self,
        conn: &Connection,
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

    /// Activate a snapshot as the current one (for new Runs). PR 1: internal
    /// and test-only — not exposed via Admin IPC.
    pub fn activate_snapshot(&self, snapshot_id: &str) -> Result<()> {
        let snap = self.load_snapshot(snapshot_id)?;
        *self.current_snapshot.lock().unwrap() = Some(snap);
        Ok(())
    }

    /// Backfill old Runs that have a NULL registry_snapshot_id with the
    /// baseline snapshot ID. Called at boot after ensure_baseline_snapshot.
    pub fn backfill_null_runs(&self) -> Result<usize> {
        let baseline = self.current_snapshot_id()?;
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("registry mutex poisoned"))?;
        let count = conn.execute(
            "UPDATE runs SET registry_snapshot_id = ?1 WHERE registry_snapshot_id IS NULL",
            params![&baseline],
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Registry {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../migrations/0001_init.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../../migrations/0002_registry_snapshots.sql"))
            .unwrap();
        Registry::new(conn).unwrap()
    }

    #[test]
    fn baseline_snapshot_created_at_boot() {
        let reg = in_memory();
        let id = reg.current_snapshot_id().unwrap();
        assert!(id.starts_with("snap_"));
        let snap = reg.load_snapshot(&id).unwrap();
        assert_eq!(snap.operations.len(), 5);
        assert!(snap.lookup("time.now").is_some());
    }

    #[test]
    fn restart_recovery_loads_same_snapshot() {
        // Simulate restart: close, reopen same DB.
        let dir = std::env::temp_dir().join(format!("reg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("reg.sqlite");

        let snap_id;
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(include_str!("../../migrations/0001_init.sql"))
                .unwrap();
            conn.execute_batch(include_str!("../../migrations/0002_registry_snapshots.sql"))
                .unwrap();
            let reg = Registry::new(conn).unwrap();
            snap_id = reg.current_snapshot_id().unwrap();
        }
        {
            let conn = Connection::open(&db_path).unwrap();
            let reg = Registry::new(conn).unwrap();
            let recovered = reg.load_snapshot(&snap_id).unwrap();
            assert_eq!(recovered.operations.len(), 5);
            assert_eq!(recovered.snapshot_id, snap_id);
            let t = recovered.lookup("time.now").unwrap();
            assert_eq!(t.risk, Risk::ReadOnly);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_snapshot_id_is_immutable() {
        let reg = in_memory();
        let id1 = reg.current_snapshot_id().unwrap();
        // Creating the same specs again returns the same ID (not a new one).
        let reg2_snap = reg.create_snapshot(builtin_specs()).unwrap();
        assert_eq!(reg2_snap.snapshot_id, id1);
    }

    #[test]
    fn different_snapshot_gets_different_id() {
        let reg = in_memory();
        let id1 = reg.current_snapshot_id().unwrap();
        let mut specs = builtin_specs();
        specs.pop(); // Remove one operation → different snapshot.
        let snap2 = reg.create_snapshot(specs).unwrap();
        assert_ne!(snap2.snapshot_id, id1);
    }

    #[test]
    fn schema_roundtrip_preserves_nested_json() {
        let reg = in_memory();
        let id = reg.current_snapshot_id().unwrap();
        let snap = reg.load_snapshot(&id).unwrap();
        let recall = snap.lookup("session.recall_recent").unwrap();
        assert_eq!(
            recall
                .parameters
                .pointer("/properties/limit/maximum")
                .and_then(|v| v.as_i64()),
            Some(20)
        );
        assert_eq!(
            recall
                .parameters
                .pointer("/additionalProperties")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn activate_snapshot_switches_current() {
        let reg = in_memory();
        let id1 = reg.current_snapshot_id().unwrap();
        let mut specs = builtin_specs();
        specs.push(OperationSpec {
            name: "new.op".into(),
            risk: Risk::ReadOnly,
            description: "new".into(),
            parameters: serde_json::json!({"type": "object"}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.new_op".into(),
        });
        let snap2 = reg.create_snapshot(specs).unwrap();
        reg.activate_snapshot(&snap2.snapshot_id).unwrap();
        assert_ne!(reg.current_snapshot_id().unwrap(), id1);
        let loaded = reg.load_snapshot(&snap2.snapshot_id).unwrap();
        assert!(loaded.lookup("new.op").is_some());
    }
}
