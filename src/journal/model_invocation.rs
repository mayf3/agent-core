//! Idempotent Journal writes for model invocation telemetry.
//!
//! A deterministic invocation id owns at most one started fact and one terminal
//! fact. Replaying the same callback returns the first durable fact; attempting
//! to change a completed invocation into a failure (or vice versa) is rejected.

use super::hash_chain::event_hash;
use super::sqlite_read::row_to_event;
use super::JournalStore;
use crate::domain::{EventId, JournalEvent, JournalEventKind, RunId, SessionId};
use anyhow::{bail, ensure, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

const STARTED: &str = "model.invocation.started.v0";
const COMPLETED: &str = "model.invocation.completed.v0";
const FAILED: &str = "model.invocation.failed.v0";

impl JournalStore {
    /// Append one versioned model invocation fact with replay-safe terminal
    /// semantics. The write and duplicate check share one IMMEDIATE
    /// transaction, so independent Kernel connections cannot create two
    /// terminal facts for the same invocation id.
    pub fn record_model_invocation_event(
        &self,
        kind: JournalEventKind,
        run_id: &RunId,
        session_id: &SessionId,
        invocation_id: &str,
        payload: Value,
    ) -> Result<JournalEvent> {
        let kind_text = kind.storage_name();
        ensure!(
            matches!(
                kind,
                JournalEventKind::ModelInvocationStarted
                    | JournalEventKind::ModelInvocationCompleted
                    | JournalEventKind::ModelInvocationFailed
            ),
            "MODEL_INVOCATION_EVENT_KIND_INVALID"
        );
        ensure!(
            !invocation_id.is_empty() && invocation_id.len() <= 256,
            "MODEL_INVOCATION_ID_INVALID"
        );
        ensure!(
            payload.get("schema_version").and_then(Value::as_str) == Some(kind_text.as_str())
                && payload.get("invocation_id").and_then(Value::as_str) == Some(invocation_id)
                && payload.get("run_id").and_then(Value::as_str) == Some(run_id.0.as_str()),
            "MODEL_INVOCATION_PAYLOAD_IDENTITY_MISMATCH"
        );

        let payload_json = serde_json::to_string(&payload)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let existing = if kind == JournalEventKind::ModelInvocationStarted {
            query_existing(&tx, invocation_id, "kind = ?2", &[STARTED])?
        } else {
            query_existing(&tx, invocation_id, "kind IN (?2, ?3)", &[COMPLETED, FAILED])?
        };
        if let Some(existing) = existing {
            ensure_same_identity(&existing, run_id, session_id)?;
            if existing.kind != kind {
                bail!("MODEL_INVOCATION_TERMINAL_CONFLICT");
            }
            tx.commit()?;
            return Ok(existing);
        }

        if kind != JournalEventKind::ModelInvocationStarted {
            let started = query_existing(&tx, invocation_id, "kind = ?2", &[STARTED])?
                .ok_or_else(|| anyhow::anyhow!("MODEL_INVOCATION_STARTED_MISSING"))?;
            ensure_same_identity(&started, run_id, session_id)?;
        } else if query_existing(&tx, invocation_id, "kind IN (?2, ?3)", &[COMPLETED, FAILED])?
            .is_some()
        {
            bail!("MODEL_INVOCATION_TERMINAL_WITHOUT_STARTED");
        }

        let event_id = EventId::new();
        let created_at = Utc::now();
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
                run_id.0,
                session_id.0,
                invocation_id,
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
            run_id: Some(run_id.clone()),
            session_id: Some(session_id.clone()),
            correlation_id: Some(invocation_id.to_string()),
            kind,
            payload,
            previous_hash,
            hash,
            created_at,
        })
    }
}

fn query_existing(
    tx: &rusqlite::Transaction<'_>,
    invocation_id: &str,
    predicate: &str,
    kinds: &[&str],
) -> Result<Option<JournalEvent>> {
    let sql = format!(
        "SELECT sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at
         FROM journal_events WHERE correlation_id = ?1 AND {predicate} ORDER BY sequence ASC LIMIT 1"
    );
    let event = match kinds {
        [kind] => tx
            .query_row(&sql, params![invocation_id, kind], row_to_event)
            .optional()?,
        [first, second] => tx
            .query_row(&sql, params![invocation_id, first, second], row_to_event)
            .optional()?,
        _ => bail!("MODEL_INVOCATION_QUERY_KIND_INVALID"),
    };
    Ok(event)
}

fn ensure_same_identity(
    event: &JournalEvent,
    run_id: &RunId,
    session_id: &SessionId,
) -> Result<()> {
    ensure!(
        event.run_id.as_ref() == Some(run_id) && event.session_id.as_ref() == Some(session_id),
        "MODEL_INVOCATION_IDENTITY_CONFLICT"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{EventObserveQuery, JournalStore};
    use serde_json::json;

    fn started(run_id: &RunId, invocation_id: &str) -> Value {
        json!({
            "schema_version": STARTED,
            "run_id": run_id.0,
            "invocation_id": invocation_id,
            "started_at": "2026-07-15T00:00:00.000Z"
        })
    }

    fn completed(run_id: &RunId, invocation_id: &str, total: u64) -> Value {
        json!({
            "schema_version": COMPLETED,
            "run_id": run_id.0,
            "invocation_id": invocation_id,
            "receipt_id": format!("model-receipt:{invocation_id}"),
            "provider": "test",
            "model": "test-model",
            "started_at": "2026-07-15T00:00:00.000Z",
            "finished_at": "2026-07-15T00:00:00.010Z",
            "latency_ms": 10,
            "input_tokens": 4,
            "cached_input_tokens": 1,
            "output_tokens": 2,
            "reasoning_tokens": 1,
            "total_tokens": total,
            "finish_reason": "stop",
            "estimated_cost": null,
            "provider_usage_extensions": {},
            "access_token": "must-not-be-observed"
        })
    }

    #[test]
    fn duplicate_terminal_callback_returns_first_durable_fact() {
        let journal = JournalStore::in_memory().unwrap();
        let run_id = RunId("run_model_duplicate".into());
        let session_id = SessionId("session_model_duplicate".into());
        let invocation_id = "model:run_model_duplicate:0";
        journal
            .record_model_invocation_event(
                JournalEventKind::ModelInvocationStarted,
                &run_id,
                &session_id,
                invocation_id,
                started(&run_id, invocation_id),
            )
            .unwrap();
        let first = journal
            .record_model_invocation_event(
                JournalEventKind::ModelInvocationCompleted,
                &run_id,
                &session_id,
                invocation_id,
                completed(&run_id, invocation_id, 6),
            )
            .unwrap();
        let duplicate = journal
            .record_model_invocation_event(
                JournalEventKind::ModelInvocationCompleted,
                &run_id,
                &session_id,
                invocation_id,
                completed(&run_id, invocation_id, 999),
            )
            .unwrap();
        assert_eq!(first.event_id, duplicate.event_id);
        assert_eq!(duplicate.payload["total_tokens"], 6);
        assert_eq!(journal.event_count().unwrap(), 2);

        let failure = json!({
            "schema_version": FAILED,
            "run_id": run_id.0,
            "invocation_id": invocation_id,
            "error_category": "late_failure"
        });
        assert!(journal
            .record_model_invocation_event(
                JournalEventKind::ModelInvocationFailed,
                &run_id,
                &session_id,
                invocation_id,
                failure,
            )
            .unwrap_err()
            .to_string()
            .contains("TERMINAL_CONFLICT"));
    }

    #[test]
    fn versioned_events_survive_restart_and_observe_preserves_usage_counters() {
        let path = std::env::temp_dir().join(format!(
            "agent_core_model_telemetry_{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let run_id = RunId("run_model_restart".into());
        let session_id = SessionId("session_model_restart".into());
        let invocation_id = "model:run_model_restart:0";
        {
            let journal = JournalStore::open(&path).unwrap();
            journal
                .record_model_invocation_event(
                    JournalEventKind::ModelInvocationStarted,
                    &run_id,
                    &session_id,
                    invocation_id,
                    started(&run_id, invocation_id),
                )
                .unwrap();
            journal
                .record_model_invocation_event(
                    JournalEventKind::ModelInvocationCompleted,
                    &run_id,
                    &session_id,
                    invocation_id,
                    completed(&run_id, invocation_id, 6),
                )
                .unwrap();
        }
        let journal = JournalStore::open(&path).unwrap();
        assert!(journal.verify_hash_chain().unwrap());
        let events = journal.events().unwrap();
        assert_eq!(events[0].kind, JournalEventKind::ModelInvocationStarted);
        assert_eq!(events[1].kind, JournalEventKind::ModelInvocationCompleted);

        let observed = journal
            .observe_events(&EventObserveQuery {
                after_sequence: None,
                limit: 10,
                event_kind: COMPLETED.into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(observed.events.len(), 1);
        assert_eq!(observed.events[0].event_kind, COMPLETED);
        assert_eq!(observed.events[0].payload["input_tokens"], 4);
        assert_eq!(observed.events[0].payload["total_tokens"], 6);
        assert_eq!(observed.events[0].payload["access_token"], "[REDACTED]");
        drop(journal);
        let _ = std::fs::remove_file(path);
    }
}
