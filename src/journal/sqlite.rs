use super::hash_chain::event_hash;
use crate::domain::*;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

pub struct JournalStore {
    conn: Mutex<Connection>,
}

impl JournalStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let store = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
        };
        store.migrate()?;
        Ok(store)
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
        conn.execute(
            "INSERT INTO runs
             (id, session_id, agent_id, trigger_event_id, principal_json, parent_run_id, delegated_by, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            ],
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

    pub fn unknown_invocations(&self) -> Result<Vec<UnknownInvocation>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT d.correlation_id, d.run_id, d.session_id, MIN(d.created_at)
             FROM journal_events d
             WHERE d.kind = 'DispatchStarted'
               AND d.correlation_id IS NOT NULL
               AND NOT EXISTS (
                 SELECT 1 FROM journal_events r
                 WHERE r.kind = 'ReceiptReceived'
                   AND r.correlation_id = d.correlation_id
               )
             GROUP BY d.correlation_id, d.run_id, d.session_id
             ORDER BY MIN(d.sequence)",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(UnknownInvocation {
                invocation_id: row.get(0)?,
                run_id: row.get::<_, Option<String>>(1)?.map(RunId),
                session_id: row.get::<_, Option<String>>(2)?.map(SessionId),
                first_dispatch_at: parse_time(row.get::<_, String>(3)?)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn recent_user_messages(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let events = self.events()?;
        let mut ingress_text_by_event = HashMap::new();
        for event in &events {
            if event.kind != JournalEventKind::IngressAccepted {
                continue;
            }
            let Some(event_id) = event.payload.get("event_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(text) = event.payload.get("text").and_then(Value::as_str) else {
                continue;
            };
            ingress_text_by_event.insert(event_id.to_string(), text.to_string());
        }
        let mut messages = vec![];
        for event in events {
            if event.kind != JournalEventKind::SessionReady
                || event.session_id.as_ref() != Some(session_id)
            {
                continue;
            }
            let Some(event_id) = event.correlation_id else {
                continue;
            };
            let Some(text) = ingress_text_by_event.get(&event_id) else {
                continue;
            };
            messages.push((event_id, text.clone()));
        }
        let start = messages.len().saturating_sub(limit);
        Ok(messages[start..].to_vec())
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

    fn migrate(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute_batch(include_str!("../../migrations/0001_init.sql"))?;
        backfill_feishu_message_dedup(&conn)?;
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

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<JournalEvent> {
    let kind_text: String = row.get(5)?;
    let payload_json: String = row.get(6)?;
    Ok(JournalEvent {
        sequence: row.get(0)?,
        event_id: EventId(row.get(1)?),
        run_id: row.get::<_, Option<String>>(2)?.map(RunId),
        session_id: row.get::<_, Option<String>>(3)?.map(SessionId),
        correlation_id: row.get(4)?,
        kind: parse_kind(&kind_text),
        payload: serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({})),
        previous_hash: row.get(7)?,
        hash: row.get(8)?,
        created_at: parse_time(row.get::<_, String>(9)?)?,
    })
}

fn parse_kind(value: &str) -> JournalEventKind {
    match value {
        "IngressAccepted" => JournalEventKind::IngressAccepted,
        "SessionReady" => JournalEventKind::SessionReady,
        "RunStarted" => JournalEventKind::RunStarted,
        "ContextBuilt" => JournalEventKind::ContextBuilt,
        "LlmCompleted" => JournalEventKind::LlmCompleted,
        "InvocationProposed" => JournalEventKind::InvocationProposed,
        "InvocationApproved" => JournalEventKind::InvocationApproved,
        "DispatchStarted" => JournalEventKind::DispatchStarted,
        "ReceiptReceived" => JournalEventKind::ReceiptReceived,
        _ => JournalEventKind::RunCompleted,
    }
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}
