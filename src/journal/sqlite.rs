use super::hash_chain::event_hash;
use super::queue::worker_job_id;
use super::sqlite_read::{parse_time, row_to_event};
use crate::domain::*;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct JournalStore {
    pub(crate) conn: Mutex<Connection>,
    /// Cached current registry snapshot ID. Set by initialize_registry at boot.
    pub(crate) current_snapshot_id: Mutex<Option<String>>,
    /// deterministic `Err`, while every other Journal operation (event append,
    /// run status update, fail_run, hash-chain verification) keeps working.
    #[cfg(any(test, feature = "test-helpers"))]
    pub(crate) recall_failure_for_test: std::sync::atomic::AtomicBool,
    /// The database path used to open this store. None for in-memory databases.
    /// Used by try_clone() for background deployment threads.
    db_path: Mutex<Option<PathBuf>>,
}

/// Raw `runs` row shape shared by the read helpers.
type RunRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
);

/// The schema `PRAGMA user_version` this kernel writes and understands. Bumped
/// only when `migrations/` gains a new applied migration. The startup
/// `migrate()` refuses to run against a DB whose version is newer than this.
const CURRENT_SCHEMA_VERSION: i64 = 20;

impl JournalStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self::with_conn(conn);
        store.set_db_path(Some(path.to_path_buf()));
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self::with_conn(conn);
        store.set_db_path(None);
        store.migrate()?;
        // Auto-init registry for tests; production uses open() + explicit init.
        store.initialize_registry()?;
        Ok(store)
    }

    /// Open a fresh connection to the same database for use in background
    /// threads. Returns an error if this is an in-memory store.
    pub fn try_clone(&self) -> Result<Self> {
        let guard = self.db_path.lock().map_err(|_| anyhow::anyhow!("mutex"))?;
        let path = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("cannot clone in-memory journal store"))?;
        Self::open(path)
    }

    pub(crate) fn set_db_path(&self, path: Option<PathBuf>) {
        if let Ok(mut guard) = self.db_path.lock() {
            *guard = path;
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn with_conn(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
            current_snapshot_id: Mutex::new(None),
            recall_failure_for_test: std::sync::atomic::AtomicBool::new(false),
            db_path: Mutex::new(None),
        }
    }

    #[cfg(not(any(test, feature = "test-helpers")))]
    fn with_conn(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
            current_snapshot_id: Mutex::new(None),
            db_path: Mutex::new(None),
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
        let kind_text = kind.storage_name();
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
        let budget_hook_id = run.budget_hook_id.as_deref();
        let budget_max_tool_rounds = run.budget_max_tool_rounds.map(|v| v as i64);
        let budget_max_wall_time_ms = run.budget_max_wall_time_ms.map(|v| v as i64);
        let budget_exhaustion_action = run.budget_exhaustion_action.map(|a| match a {
            crate::hook::ExhaustionAction::Terminate => "terminate",
            crate::hook::ExhaustionAction::Yield => "yield",
        });
        conn.execute(
            "INSERT INTO runs
             (id, session_id, agent_id, trigger_event_id, principal_json, parent_run_id, delegated_by, status, created_at, updated_at, registry_snapshot_id, mode, budget_hook_id, budget_max_tool_rounds, budget_max_wall_time_ms, budget_exhaustion_action)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                budget_hook_id,
                budget_max_tool_rounds,
                budget_max_wall_time_ms,
                budget_exhaustion_action,
            ],
        )?;
        Ok(())
    }

    /// Insert a Run row AND append its `RunStarted` event in ONE transaction
    /// (High 4). A crash between the two otherwise left a Run row present but
    /// with no `RunStarted`, which the worker mistook for an already-completed
    /// Run and returned success — losing the continuation forever. This closes
    /// that window: both facts commit together or not at all.
    ///
    pub fn insert_run_and_start(
        &self,
        run: &Run,
        session_id: &SessionId,
        correlation_id: &str,
        trigger_run_id: &RunId,
    ) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        super::queue::insert_run_tx(&tx, run)?;
        let payload = json!({
            "run_id": run.id.0,
            "trigger_event_id": run.trigger_event_id.0,
            "principal_id": run.principal.principal_id.0,
            "continuation_of": trigger_run_id.0,
        });
        super::queue::append_event_tx(
            &tx,
            JournalEventKind::RunStarted,
            Some(&run.id),
            Some(session_id),
            Some(correlation_id),
            payload,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether a `RunStarted` journal event exists for a Run (High 4). This is
    /// the lifecycle fact that distinguishes "a Run row merely exists" from
    /// "a Run actually started". The worker idempotency path uses it so a
    /// half-created Run (row present, RunStarted absent — the pre-fix crash
    /// window) fails closed instead of being re-driven or silently treated as
    /// success.
    pub fn run_has_started(&self, run_id: &RunId) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM journal_events WHERE run_id = ?1 AND kind = 'RunStarted'",
            params![run_id.0],
            |row| row.get(0),
        )?;
        Ok(count > 0)
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

    /// Whether the Run ended with a budget exhaustion that carried the
    /// `yield` exhaustion action (a structured yield fact). Used by the
    /// delivery path to avoid fabricating a "please send 继续" user reply —
    /// the external Agent Loop Harness observes this fact instead.
    pub fn run_yielded(&self, run_id: &RunId) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM journal_events
             WHERE run_id = ?1
               AND kind IN ('ToolBudgetExhausted', 'ToolLoopWallClockExceeded')
               AND json_extract(payload_json, '$.exhaustion_action') = 'yield'",
            params![run_id.0],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Load a Run by ID as a generic governance fact. Used by the
    /// `/v1/session-continuation` seam to verify the trigger Run and recover
    /// its session / principal so the continuation resolves to the SAME
    /// session. Read-only; the Kernel never interprets the Run's content.
    pub fn run_by_id(&self, run_id: &RunId) -> Result<Option<Run>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row: Option<RunRow> = conn
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
        let principal: RunPrincipal = serde_json::from_str(&principal_json)?;
        let created_at: String = created_at;
        let updated_at: String = updated_at;
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
            created_at: parse_time(created_at)?,
            updated_at: parse_time(updated_at)?,
            registry_snapshot_id: registry_snapshot_id.unwrap_or_default(),
            mode: RunMode::Default,
            budget_hook_id: None,
            budget_hook_version: None,
            budget_decision_digest: None,
            budget_max_tool_rounds: None,
            budget_max_wall_time_ms: None,
            budget_exhaustion_action: None,
        }))
    }

    /// Load a Session by ID as a generic governance fact. Used by the
    /// `/v1/session-continuation` seam to rebuild the session target so the
    /// continuation resolves to the SAME session row.
    pub fn session_by_id(&self, session_id: &SessionId) -> Result<Option<Session>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT id, agent_id, channel, conversation_key, last_active_at, status, version
             FROM sessions WHERE id = ?1",
            params![session_id.0],
            |row| {
                let status: String = row.get(5)?;
                Ok(Session {
                    id: SessionId(row.get(0)?),
                    agent_id: AgentId(row.get(1)?),
                    channel: match row.get::<_, String>(2)?.as_str() {
                        "Cli" => ChannelKind::Cli,
                        "Feishu" => ChannelKind::Feishu,
                        other => {
                            return Err(rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!("unknown channel: {other}"),
                                )),
                            ));
                        }
                    },
                    conversation_key: row.get(3)?,
                    summary: None,
                    summarized_until_event_id: None,
                    last_active_at: parse_time(row.get::<_, String>(4)?)?,
                    status: if status == "Archived" {
                        SessionStatus::Archived
                    } else {
                        SessionStatus::Active
                    },
                    version: row.get::<_, i64>(6)? as u64,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Atomically accept a same-session continuation (High 4): in ONE
    /// transaction, PRE-ALLOCATE the next_run_id, record the generic
    /// `SessionContinuationRequested` governance event (NOT an
    /// IngressAccepted / user message), write the ledger row with the
    /// pre-allocated next_run_id, and enqueue a `schedule_continuation` worker
    /// job. All four facts succeed or roll back together — the ledger is the
    /// single trusted fact.
    ///
    /// Returns `Ok(Some((event_id, next_run_id)))` on first acceptance, and
    /// `Ok(None)` for a duplicate trigger (nothing new is created — the
    /// already-accepted continuation is returned via
    /// [`JournalStore::continuation_by_trigger_run`]).
    ///
    /// The Kernel does NOT decide whether the continuation should happen; it
    /// only verifies generic governance facts and records the fact.
    pub fn accept_session_continuation(
        &self,
        request_id: &str,
        session_id: &SessionId,
        trigger_run_id: &RunId,
        requesting_principal: &str,
        idempotency_key: &str,
        next_run_id: &RunId,
    ) -> Result<Option<(String, String)>> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Idempotency: a trigger Run may be continued at most once. This check
        // runs BEFORE any insert so a duplicate trigger creates NOTHING new.
        // UNIQUE(trigger_run_id) remains the hard guarantee against races.
        let existing: Option<String> = tx
            .query_row(
                "SELECT event_id FROM session_continuations WHERE trigger_run_id = ?1",
                params![trigger_run_id.0],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            drop(tx);
            return Ok(None);
        }
        // Generic governance event: an authorized caller requested the next Run
        // in the same session based on a trigger Run. This is NOT a user
        // message and carries no product semantics.
        let appended = super::queue::append_event_tx(
            &tx,
            JournalEventKind::SessionContinuationRequested,
            None,
            Some(session_id),
            Some(request_id),
            json!({
                "request_id": request_id,
                "trigger_run_id": trigger_run_id.0,
                "session_id": session_id.0,
                "requesting_principal": requesting_principal,
                "idempotency_key": idempotency_key,
                "next_run_id": next_run_id.0,
            }),
        )?;
        let event_id = appended.event_id;
        // The ledger row is inserted FIRST with the pre-allocated next_run_id;
        // UNIQUE(trigger_run_id) fails this INSERT (rolling back everything)
        // if a concurrent request won the race.
        tx.execute(
            "INSERT INTO session_continuations
             (idempotency_key, session_id, trigger_run_id, event_id, next_run_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                idempotency_key,
                session_id.0,
                trigger_run_id.0,
                event_id.0,
                next_run_id.0,
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO worker_jobs
             (job_id, job_type, source_event_id, status, attempts, available_at, created_at, updated_at)
             VALUES (?1, 'schedule_continuation', ?2, ?3, 0, ?4, ?4, ?4)",
            params![
                worker_job_id(&event_id).as_str(),
                event_id.0.as_str(),
                WorkerJobStatus::Queued.as_str(),
                now.as_str(),
            ],
        )?;
        tx.commit()?;
        Ok(Some((event_id.0.clone(), next_run_id.0.clone())))
    }

    /// Load the accepted continuation for a trigger Run. Returns the
    /// continuation event_id and the PRE-ALLOCATED next_run_id. The
    /// next_run_id is always present (pre-allocated at acceptance time), so a
    /// duplicate request can immediately return the SAME next_run_id without
    /// waiting for the worker.
    pub fn continuation_by_trigger_run(
        &self,
        trigger_run_id: &RunId,
    ) -> Result<Option<(String, String)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT event_id, next_run_id FROM session_continuations WHERE trigger_run_id = ?1",
                params![trigger_run_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Load the generic `SessionContinuationRequested` journal event by its
    /// event_id. Used by the worker loop to recover the trigger Run reference
    /// for a `schedule_continuation` worker job. This is NOT an ingress event
    /// and carries no user message.
    pub fn continuation_request_by_event_id(
        &self,
        event_id: &str,
    ) -> Result<Option<JournalEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at
             FROM journal_events
             WHERE event_id = ?1 AND kind = 'SessionContinuationRequested'
             ORDER BY sequence DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![event_id], row_to_event)?;
        Ok(rows.next().transpose()?)
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
            let kind_text = event.kind.storage_name();
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
            conn.execute_batch(include_str!("../../migrations/0013_component_registry.sql"))?;
            conn.execute_batch(include_str!(
                "../../migrations/0014_external_receipt_envelope_digests.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0015_delivery_manifest_columns.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0016_hcr_failure_reconciliation.sql"
            ))?;
            conn.execute_batch(include_str!(
                "../../migrations/0017_generic_acceptance_receipts.sql"
            ))?;
            ensure_budget_columns(&conn)?;
            ensure_registry_hook_bindings_table(&conn)?;
            super::queue::migrate(&conn)?;
            backfill_feishu_message_dedup(&conn)?;
            conn.execute_batch(include_str!(
                "../../migrations/0020_session_continuations.sql"
            ))?;
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
                12 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0013_component_registry.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 13)?;
                }
                13 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0014_external_receipt_envelope_digests.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 14)?;
                }
                14 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0015_delivery_manifest_columns.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 15)?;
                }
                15 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0016_hcr_failure_reconciliation.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 16)?;
                }
                16 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0017_generic_acceptance_receipts.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 17)?;
                }
                17 => {
                    ensure_budget_columns(&conn)?;
                    conn.pragma_update(None, "user_version", 18)?;
                }
                18 => {
                    ensure_registry_hook_bindings_table(&conn)?;
                    conn.pragma_update(None, "user_version", 19)?;
                }
                19 => {
                    conn.execute_batch(include_str!(
                        "../../migrations/0020_session_continuations.sql"
                    ))?;
                    conn.pragma_update(None, "user_version", 20)?;
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

/// Idempotently add the Run Budget Hook V0 columns to the `runs` table.
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, so we check `PRAGMA table_info`
/// before each ALTER. Safe to call on fresh and existing databases, and safe
/// to re-run after a manual version downgrade + reopen (the migration loop
/// test scenario).
pub(crate) fn ensure_budget_columns(conn: &Connection) -> Result<()> {
    for (col, decl) in [
        ("budget_hook_id", "TEXT"),
        ("budget_max_tool_rounds", "INTEGER"),
        ("budget_max_wall_time_ms", "INTEGER"),
        ("budget_exhaustion_action", "TEXT"),
    ] {
        let exists: bool = {
            let mut stmt = conn.prepare("PRAGMA table_info(runs)")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = false;
            for row in rows {
                if row? == col {
                    found = true;
                    break;
                }
            }
            found
        };
        if !exists {
            conn.execute_batch(&format!("ALTER TABLE runs ADD COLUMN {col} {decl};"))?;
        }
    }
    Ok(())
}

/// Idempotently add the registry budget hook binding columns to the
/// `registry_snapshots` table. Same guard pattern as `ensure_budget_columns`;
/// safe to re-run after a manual version downgrade + reopen.
pub(crate) fn ensure_registry_hook_bindings_table(conn: &Connection) -> Result<()> {
    let exists: bool = {
        let mut stmt = conn.prepare(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='registry_snapshot_hook_bindings'",
        )?;
        let count: i64 = stmt.query_row([], |row| row.get(0))?;
        count > 0
    };
    if !exists {
        conn.execute_batch(include_str!(
            "../../migrations/0019_registry_hook_bindings.sql"
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod run_start_atomic_tests {
    use super::*;

    fn run(id: &str) -> Run {
        let now = Utc::now();
        Run {
            id: RunId(id.to_string()),
            session_id: SessionId("session_atomic".into()),
            agent_id: AgentId("agent_atomic".into()),
            trigger_event_id: EventId("event_atomic".into()),
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: Some("cli:local".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
            registry_snapshot_id: String::new(),
            mode: RunMode::Default,
            budget_hook_id: None,
            budget_hook_version: None,
            budget_decision_digest: None,
            budget_max_tool_rounds: None,
            budget_max_wall_time_ms: None,
            budget_exhaustion_action: None,
        }
    }

    #[test]
    fn run_row_rolls_back_when_run_started_insert_fails() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        {
            let conn = journal
                .conn
                .lock()
                .map_err(|_| anyhow!("journal mutex poisoned"))?;
            conn.execute_batch(
                "CREATE TRIGGER force_run_started_failure
                 BEFORE INSERT ON journal_events
                 WHEN NEW.kind = 'RunStarted'
                 BEGIN
                   SELECT RAISE(ABORT, 'forced RunStarted failure');
                 END;",
            )?;
        }
        let run = run("run_atomic_rollback");
        let result = journal.insert_run_and_start(
            &run,
            &run.session_id,
            "continuation_atomic_event",
            &RunId("run_trigger_atomic".into()),
        );
        assert!(result.is_err(), "forced RunStarted failure must surface");
        assert!(
            journal.run_by_id(&run.id)?.is_none(),
            "Run row must roll back with RunStarted"
        );
        assert!(!journal.run_has_started(&run.id)?);
        Ok(())
    }
}
