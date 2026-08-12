//! Model-independent chat control plane owned by the Agent Host.
//!
//! Command parsing and presentation remain in connectors. This module accepts
//! a normalized channel/session target over authenticated IPC and performs
//! only mechanical Session/Run/Journal operations.

use crate::config::KernelConfig;
use crate::domain::{ChannelKind, RunId, SessionId, SessionTarget};
use crate::journal::{CancellationRequestOutcome, JournalStore};
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "chat-control.v0";

#[derive(Debug, Deserialize)]
pub(crate) struct ChatControlRequest {
    protocol_version: String,
    action: String,
    channel: String,
    conversation_key: String,
    requester_id: String,
}

pub(crate) fn handle(config: &KernelConfig, journal: &JournalStore, body: &[u8]) -> Result<Value> {
    let request: ChatControlRequest = serde_json::from_slice(body)?;
    if request.protocol_version != PROTOCOL_VERSION {
        bail!("unsupported_chat_control_version");
    }
    if request.conversation_key.trim().is_empty() || request.requester_id.trim().is_empty() {
        bail!("invalid_chat_control_target");
    }
    let channel = match request.channel.as_str() {
        "Cli" => ChannelKind::Cli,
        "Feishu" => ChannelKind::Feishu,
        _ => bail!("unsupported_chat_control_channel"),
    };
    let target = SessionTarget {
        agent_id: config.agent_id.clone(),
        channel,
        conversation_key: request.conversation_key,
    };
    match request.action.as_str() {
        "status" => status(journal, &target),
        "stop" => stop(journal, &target, &request.requester_id),
        "reset" => reset(journal, &target),
        _ => bail!("unsupported_chat_control_action"),
    }
}

fn status(journal: &JournalStore, target: &SessionTarget) -> Result<Value> {
    let Some(session_id) = active_session_id(journal, target)? else {
        return Ok(json!({
            "ok": true,
            "protocol_version": PROTOCOL_VERSION,
            "action": "status",
            "session_id": null,
            "run": null,
        }));
    };
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow!("journal mutex poisoned"))?;
    let row: Option<(String, String, String, String, Option<i64>, String)> = conn
        .query_row(
            "SELECT id, status, created_at, updated_at, budget_max_tool_rounds, trigger_event_id
             FROM runs WHERE session_id = ?1 ORDER BY created_at DESC LIMIT 1",
            params![session_id.0],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((run_id, run_status, created_at, updated_at, max_tools, trigger_event_id)) = row
    else {
        return Ok(json!({
            "ok": true,
            "protocol_version": PROTOCOL_VERSION,
            "action": "status",
            "session_id": session_id.0,
            "run": null,
        }));
    };
    let tool_calls: i64 = conn.query_row(
        "SELECT COUNT(*) FROM journal_events WHERE run_id = ?1 AND kind = 'ToolCallIssued'",
        params![run_id],
        |row| row.get(0),
    )?;
    let task: Option<String> = conn
        .query_row(
            "SELECT json_extract(payload_json, '$.text') FROM journal_events
             WHERE kind = 'IngressAccepted'
               AND json_extract(payload_json, '$.event_id') = ?1
             ORDER BY sequence DESC LIMIT 1",
            params![trigger_event_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let latest: Option<(String, String, String)> = conn
        .query_row(
            "SELECT kind, payload_json, created_at FROM journal_events
             WHERE run_id = ?1
               AND kind IN ('model.invocation.started.v0', 'model.invocation.completed.v0',
                            'model.invocation.failed.v0', 'ToolCallIssued', 'ReceiptReceived',
                            'RunFailed', 'RunCancellationRequested', 'RunCancelled')
             ORDER BY sequence DESC LIMIT 1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let changed_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM journal_events
         WHERE run_id = ?1 AND kind = 'ToolCallIssued'
           AND (json_extract(payload_json, '$.operation') LIKE '%workspace_write%'
                OR json_extract(payload_json, '$.operation') IN
                   ('external.execution_task', 'external.coding_task_submit'))",
        params![run_id],
        |row| row.get(0),
    )?;
    let terminal_payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM journal_events
             WHERE run_id = ?1 AND kind IN ('RunFailed', 'RunCancelled')
             ORDER BY sequence DESC LIMIT 1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()?;
    drop(conn);

    let terminal = terminal_payload
        .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
        .unwrap_or(Value::Null);
    let failure_reason = terminal
        .get("reason")
        .or_else(|| terminal.get("error_category"))
        .and_then(Value::as_str);
    let (activity_kind, activity_detail, activity_at) = latest
        .map(|(kind, payload, at)| {
            let payload = serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null);
            let detail = payload
                .get("operation")
                .or_else(|| payload.get("status"))
                .or_else(|| payload.get("error_category"))
                .and_then(Value::as_str)
                .map(str::to_string);
            (Some(kind), detail, Some(at))
        })
        .unwrap_or((None, None, None));
    let created = parse_time(&created_at)?;
    let end = if is_terminal_status(&run_status) {
        parse_time(&updated_at)?
    } else {
        Utc::now()
    };
    let elapsed_ms = (end - created).num_milliseconds().max(0);
    Ok(json!({
        "ok": true,
        "protocol_version": PROTOCOL_VERSION,
        "action": "status",
        "session_id": session_id.0,
        "run": {
            "run_id": run_id,
            "status": run_status,
            "task": task,
            "elapsed_ms": elapsed_ms,
            "tool_calls_used": tool_calls,
            "tool_calls_max": max_tools,
            "recent_activity": {
                "kind": activity_kind,
                "detail": activity_detail,
                "at": activity_at,
            },
            "code_modified": changed_count > 0,
            "failure_reason": failure_reason,
        },
    }))
}

fn stop(journal: &JournalStore, target: &SessionTarget, requester_id: &str) -> Result<Value> {
    let Some(session_id) = active_session_id(journal, target)? else {
        return Ok(stop_response(None, "no_active_session"));
    };
    let latest: Option<(String, String)> = {
        let conn = journal
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT id, status FROM runs WHERE session_id = ?1 ORDER BY created_at DESC LIMIT 1",
            params![session_id.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
    };
    let Some((run_id, _)) = latest else {
        return Ok(stop_response(None, "no_run"));
    };
    let run_id = RunId(run_id);
    let outcome = journal.request_run_cancellation(&run_id, requester_id)?;
    let state = match outcome {
        CancellationRequestOutcome::Requested => "cancellation_requested",
        CancellationRequestOutcome::AlreadyRequested => "already_requested",
        CancellationRequestOutcome::AlreadyCancelled => "already_cancelled",
        CancellationRequestOutcome::NotCancellable(_) => "not_running",
        CancellationRequestOutcome::RunNotFound => "run_not_found",
    };
    Ok(stop_response(Some(&run_id.0), state))
}

fn stop_response(run_id: Option<&str>, state: &str) -> Value {
    json!({
        "ok": true,
        "protocol_version": PROTOCOL_VERSION,
        "action": "stop",
        "run_id": run_id,
        "state": state,
    })
}

fn reset(journal: &JournalStore, target: &SessionTarget) -> Result<Value> {
    let now = Utc::now();
    let new_session_id = SessionId::new();
    let mut conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow!("journal mutex poisoned"))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT id FROM sessions
             WHERE agent_id = ?1 AND channel = ?2 AND conversation_key = ?3
             ORDER BY last_active_at DESC LIMIT 1",
            params![
                target.agent_id.0,
                format!("{:?}", target.channel),
                target.conversation_key
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(previous_id) = previous.as_deref() {
        let archived_key = format!("{}#archived:{}", target.conversation_key, previous_id);
        tx.execute(
            "UPDATE sessions SET status = 'Archived', conversation_key = ?1,
                                 last_active_at = ?2, version = version + 1
             WHERE id = ?3",
            params![archived_key, now.to_rfc3339(), previous_id],
        )?;
    }
    tx.execute(
        "INSERT INTO sessions
         (id, agent_id, channel, conversation_key, summary,
          summarized_until_event_id, last_active_at, status, version)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, 'Active', 1)",
        params![
            new_session_id.0,
            target.agent_id.0,
            format!("{:?}", target.channel),
            target.conversation_key,
            now.to_rfc3339(),
        ],
    )?;
    tx.commit()?;
    Ok(json!({
        "ok": true,
        "protocol_version": PROTOCOL_VERSION,
        "action": "reset",
        "previous_session_id": previous,
        "session_id": new_session_id.0,
        "history_deleted": false,
    }))
}

fn active_session_id(journal: &JournalStore, target: &SessionTarget) -> Result<Option<SessionId>> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow!("journal mutex poisoned"))?;
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM sessions
             WHERE agent_id = ?1 AND channel = ?2 AND conversation_key = ?3
               AND status = 'Active'
             ORDER BY last_active_at DESC LIMIT 1",
            params![
                target.agent_id.0,
                format!("{:?}", target.channel),
                target.conversation_key
            ],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id.map(SessionId))
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "Completed" | "Failed" | "Cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentId, EventId, PrincipalId, PrincipalSource, PrincipalSubject, Run, RunMode,
        RunPrincipal, RunStatus,
    };

    fn config() -> KernelConfig {
        KernelConfig::from_cli(None)
    }

    fn target() -> SessionTarget {
        SessionTarget {
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:test".into(),
        }
    }

    fn running_run(journal: &JournalStore, session_id: SessionId) -> Result<Run> {
        let now = Utc::now();
        let run = Run {
            id: RunId::new(),
            session_id,
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:test".into()),
                subject: PrincipalSubject::FeishuOpenId("test".into()),
                source: PrincipalSource::Feishu,
                grants: vec![],
                requester_id: Some("feishu:open_id:test".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
            registry_snapshot_id: "snap".into(),
            mode: RunMode::Default,
            budget_hook_id: None,
            budget_hook_version: None,
            budget_decision_digest: None,
            budget_max_tool_rounds: Some(64),
            budget_max_wall_time_ms: None,
            budget_exhaustion_action: None,
        };
        journal.insert_run(&run)?;
        Ok(run)
    }

    #[test]
    fn status_stop_and_reset_bypass_model_and_preserve_history() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let session = journal.get_or_create_session(&target())?;
        let run = running_run(&journal, session.id.clone())?;

        let status_body = serde_json::to_vec(&json!({
            "protocol_version": PROTOCOL_VERSION,
            "action": "status",
            "channel": "Feishu",
            "conversation_key": target().conversation_key,
            "requester_id": "feishu:open_id:test",
        }))?;
        let snapshot = handle(&config(), &journal, &status_body)?;
        assert_eq!(snapshot["run"]["run_id"], run.id.0);
        assert_eq!(snapshot["run"]["status"], "Running");

        let stop_body = serde_json::to_vec(&json!({
            "protocol_version": PROTOCOL_VERSION,
            "action": "stop",
            "channel": "Feishu",
            "conversation_key": target().conversation_key,
            "requester_id": "feishu:open_id:test",
        }))?;
        let stopped = handle(&config(), &journal, &stop_body)?;
        assert_eq!(stopped["state"], "cancellation_requested");

        let reset_body = serde_json::to_vec(&json!({
            "protocol_version": PROTOCOL_VERSION,
            "action": "reset",
            "channel": "Feishu",
            "conversation_key": target().conversation_key,
            "requester_id": "feishu:open_id:test",
        }))?;
        let reset_result = handle(&config(), &journal, &reset_body)?;
        assert_eq!(reset_result["history_deleted"], false);
        assert_ne!(reset_result["session_id"], session.id.0);
        assert!(journal.session_by_id(&session.id)?.is_some());
        assert_eq!(
            journal.run_status(&run.id)?.as_deref(),
            Some("CancellationRequested")
        );
        Ok(())
    }
}
