use super::hash_chain::event_hash;
use super::sqlite_read::{parse_time, row_to_event};
use crate::domain::*;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;

pub struct JournalStore {
    pub(crate) conn: Mutex<Connection>,
    /// Cached current registry snapshot ID. Set by initialize_registry at boot.
    pub(crate) current_snapshot_id: Mutex<Option<String>>,
    /// deterministic `Err`, while every other Journal operation (event append,
    /// run status update, fail_run, hash-chain verification) keeps working.
    #[cfg(any(test, feature = "test-helpers"))]
    pub(crate) recall_failure_for_test: std::sync::atomic::AtomicBool,
}

/// The schema `PRAGMA user_version` this kernel writes and understands. Bumped
/// only when `migrations/` gains a new applied migration. The startup
/// `migrate()` refuses to run against a DB whose version is newer than this.
const CURRENT_SCHEMA_VERSION: i64 = 12;

impl JournalStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self::with_conn(conn);
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self::with_conn(conn);
        store.migrate()?;
        // Auto-init registry for tests; production uses open() + explicit init.
        store.initialize_registry()?;
        Ok(store)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn with_conn(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
            current_snapshot_id: Mutex::new(None),
            recall_failure_for_test: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(not(any(test, feature = "test-helpers")))]
    fn with_conn(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
            current_snapshot_id: Mutex::new(None),
        }
    }
    /// The applied schema version (`PRAGMA user_version`). Useful for
    /// operators and tests to confirm which migration level a database is at.
    pub fn schema_version(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        Ok(conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?)
    }

    pub fn append_event(
        &self,
        kind: JournalEventKind,
        run_id: Option<&RunId>,
        session_id: Option<&SessionId>,
        correlation_id: Option<&str>,
        payload: Value,
    ) -> Result<JournalEvent> {
        let event_id = EventId::new();
        let created_at = Utc::now();
        let payload_json = serde_json::to_string(&payload)?;
        let kind_text = format!("{:?}", kind);
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous = tx
            .query_row(
                "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let sequence = previous.as_ref().map(|(seq, _)| seq + 1).unwrap_or(1);
        let previous_hash = previous.map(|(_, hash)| hash);
        let hash = event_hash(
            previous_hash.as_deref(),
            sequence,
            &kind_text,
            &payload_json,
        );
        tx.execute(
            "INSERT INTO journal_events
             (sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                sequence,
                event_id.0,
                run_id.map(|id| id.0.as_str()),
                session_id.map(|id| id.0.as_str()),
                correlation_id,
                kind_text,
                payload_json,
                previous_hash,
                hash,
                created_at.to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(JournalEvent {
            sequence,
            event_id,
            run_id: run_id.cloned(),
            session_id: session_id.cloned(),
            correlation_id: correlation_id.map(str::to_string),
            kind,
            payload,
            previous_hash,
            hash,
            created_at,
        })
    }

    pub fn reserve_ingress(
        &self,
        source: &str,
        external_event_id: &str,
        event_id: &EventId,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let changed = conn.execute(
            "INSERT OR IGNORE INTO ingress_dedup (source, external_event_id, event_id, first_seen_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![source, external_event_id, event_id.0, Utc::now().to_rfc3339()],
        )?;
        Ok(changed == 1)
    }

    pub fn get_or_create_session(&self, target: &SessionTarget) -> Result<Session> {
        if let Some(session) = self.find_session(target)? {
            return Ok(session);
        }
        let session = Session {
            id: SessionId::new(),
            agent_id: target.agent_id.clone(),
            channel: target.channel.clone(),
            conversation_key: target.conversation_key.clone(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
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
                Option::<String>::None,
                session.last_active_at.to_rfc3339(),
                format!("{:?}", session.status),
                session.version,
            ],
        )?;
        Ok(session)
    }

    pub fn insert_run(&self, run: &Run) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mode_str = serde_json::to_string(&run.mode)?;
        conn.execute(
            "INSERT INTO runs
             (id, session_id, agent_id, trigger_event_id, principal_json, parent_run_id, delegated_by, status, created_at, updated_at, registry_snapshot_id, mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                run.id.0,
                run.session_id.0,
                run.agent_id.0,
                run.trigger_event_id.0,
                serde_json::to_string(&run.principal)?,
                run.parent_run_id.as_ref().map(|id| id.0.as_str()),
                run.delegated_by.as_ref().map(|id| id.0.as_str()),
                format!("{:?}", run.status),
                run.created_at.to_rfc3339(),
                run.updated_at.to_rfc3339(),
                if run.registry_snapshot_id.is_empty() {
                    None
                } else {
                    Some(&run.registry_snapshot_id)
                },
                mode_str,
            ],
        )?;
        Ok(())
    }

    pub fn update_run_status(&self, run_id: &RunId, status: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE runs SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, Utc::now().to_rfc3339(), run_id.0],
        )?;
        Ok(())
    }

    pub fn complete_run(&self, run_id: &RunId) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE runs SET status = 'Completed', updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), run_id.0],
        )?;
        Ok(())
    }

    pub fn fail_run(&self, run_id: &RunId) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE runs SET status = 'Failed', updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), run_id.0],
        )?;
        Ok(())
    }

    pub fn run_status(&self, run_id: &RunId) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM runs WHERE id = ?1",
                params![run_id.0],
                |row| row.get(0),
            )
            .optional()?;
        Ok(status)
    }

    pub fn events(&self) -> Result<Vec<JournalEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at
             FROM journal_events ORDER BY sequence",
        )?;
        let rows = stmt.query_map([], row_to_event)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn event_count(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row("SELECT COUNT(*) FROM journal_events", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn verify_hash_chain(&self) -> Result<bool> {
        let events = self.events()?;
        let mut previous_hash: Option<String> = None;
        for event in events {
            let payload_json = serde_json::to_string(&event.payload)?;
            let kind_text = format!("{:?}", event.kind);
            let expected = event_hash(
                previous_hash.as_deref(),
                event.sequence,
                &kind_text,
                &payload_json,
            );
            if event.previous_hash != previous_hash || event.hash != expected {
                return Ok(false);
            }
            previous_hash = Some(event.hash);
        }
        Ok(true)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let applied = conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
        if applied > CURRENT_SCHEMA_VERSION {
            // The on-disk DB is newer than this kernel binary understands.
            // Bail loudly with a sanitized, version-only message so an
            // operator knows to upgrade the kernel rather than letting a
            // partial/old migration run and corrupt the schema. (Phase 1
            // hardening: migration check.)
            bail!(
                "database schema version {applied} is newer than supported version {CURRENT_SCHEMA_VERSION}; upgrade the kernel"
            );
        }
        if applied == 0 {
            // Fresh database: run all migrations and stamp current version.
            conn.execute_batch(include_str!("../../migrations/0001_init.sql"))?;
            conn.execute_batch(include_str!("../../migrations/0002_registry_snapshots.sql"))?;
            conn.execute_batch(include_str!(
                "../../migrations/0003_external_harness_hotload.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0004_capability_change_proposals.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0005_remove_manifest_operation_name_unique.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0006_external_operation_grants.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0007_harness_change_requests.sql"
            ))?;
            conn.execute_batch(include_str!("../../migrations/0008_hcr_claims.sql"))?;
            conn.execute_batch(include_str!("../../migrations/0009_hcr_evidence.sql"))?;
            conn.execute_batch(include_str!(
                "../../migrations/0010_hcr_receipt_identity.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0011_capability_proposal_hcr_links.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0012_capability_change_approvals.sql"
            ))?;
            super::queue::migrate(&conn)?;
            backfill_feishu_message_dedup(&conn)?;
            conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        } else if applied == 1 {
            conn.execute_batch(include_str!("../../migrations/0002_registry_snapshots.sql"))?;
            super::queue::migrate(&conn)?;
            backfill_feishu_message_dedup(&conn)?;
            conn.pragma_update(None, "user_version", 2)?;
            // Fall through to v2→v3→v4.
        }
        // Apply any pending version upgrades after the initial v0/v1 blocks.
        loop {
            let current = conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            if current >= CURRENT_SCHEMA_VERSION {
                break;
            }
            match current {
                2 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0003_external_harness_hotload.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 3)?;
                }
                3 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0004_capability_change_proposals.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 4)?;
                }
                4 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0005_remove_manifest_operation_name_unique.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 5)?;
                }
                5 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0006_external_operation_grants.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 6)?;
                }
                6 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0007_harness_change_requests.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 7)?;
                }
                7 => {
                    conn.execute_batch(include_str!("../../migrations/0008_hcr_claims.sql"))?;
                    conn.pragma_update(None, "user_version", 8)?;
                }
                8 => {
                    conn.execute_batch(include_str!("../../migrations/0009_hcr_evidence.sql"))?;
                    conn.pragma_update(None, "user_version", 9)?;
                }
                9 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0010_hcr_receipt_identity.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 10)?;
                }
                10 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0011_capability_proposal_hcr_links.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 11)?;
                }
                11 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0012_capability_change_approvals.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 12)?;
                }
                _ => break,
            }
        }
        if applied >= 1 {
            // Existing database at a known version: the base schema migration
            // is already applied. queue::migrate and the dedup backfill are
            // idempotent / read-only-safe, so they can run every startup to
            // heal any projection drift.
            super::queue::migrate(&conn)?;
            backfill_feishu_message_dedup(&conn)?;
        }
        Ok(())
    }

    fn find_session(&self, target: &SessionTarget) -> Result<Option<Session>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT id, last_active_at, status, version FROM sessions
             WHERE agent_id = ?1 AND channel = ?2 AND conversation_key = ?3",
            params![
                target.agent_id.0,
                format!("{:?}", target.channel),
                target.conversation_key
            ],
            |row| {
                let status: String = row.get(2)?;
                Ok(Session {
                    id: SessionId(row.get(0)?),
                    agent_id: target.agent_id.clone(),
                    channel: target.channel.clone(),
                    conversation_key: target.conversation_key.clone(),
                    summary: None,
                    summarized_until_event_id: None,
                    last_active_at: parse_time(row.get::<_, String>(1)?)?,
                    status: if status == "Archived" {
                        SessionStatus::Archived
                    } else {
                        SessionStatus::Active
                    },
                    version: row.get::<_, i64>(3)? as u64,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }
}

fn backfill_feishu_message_dedup(conn: &Connection) -> Result<()> {
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT event_id, payload_json, created_at
             FROM journal_events
             WHERE kind = 'IngressAccepted'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (event_id, payload_json, created_at) in rows {
        let Ok(payload) = serde_json::from_str::<Value>(&payload_json) else {
            continue;
        };
        if payload.get("source").and_then(Value::as_str) != Some("feishu") {
            continue;
        }
        let Some(message_id) = payload
            .get("message_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        conn.execute(
            "INSERT OR IGNORE INTO ingress_dedup (source, external_event_id, event_id, first_seen_at)
             VALUES (?1, ?2, ?3, ?4)",
            params!["feishu", format!("message:{message_id}"), event_id, created_at],
        )?;
    }
    Ok(())
}
