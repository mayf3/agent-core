//! Session conversation turn history for context assembly.
//!
//! Uses explicit `AssistantReplyDelivered` journal events paired by run_id.
//! No guessing from arbitrary `ReceiptReceived.output` values.
//! `recent_user_messages` (legacy) returns user-only messages for the
//! `session.recall_recent` capability.

use crate::domain::{JournalEventKind, SessionId};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

impl super::JournalStore {
    /// Return a bounded, verbatim projection of the immediately preceding
    /// unfinished Run for an explicit user-driven continuation. This is not a
    /// summary artifact and never advances `summarized_until_event_id`: it is
    /// only recent durable Journal facts (goal + tool/result activity).
    pub(crate) fn previous_unfinished_run_context(
        &self,
        session_id: &SessionId,
        current_event_id: &str,
    ) -> Result<Option<String>> {
        let events = self.events()?;
        let previous = events.iter().rev().find(|event| {
            event.kind == JournalEventKind::RunStarted
                && event.session_id.as_ref() == Some(session_id)
                && event.correlation_id.as_deref() != Some(current_event_id)
                && event.run_id.is_some()
        });
        let Some(run_id) = previous.and_then(|event| event.run_id.as_ref()) else {
            return Ok(None);
        };
        let status = self.run_status(run_id)?.unwrap_or_else(|| "Unknown".to_string());
        if !matches!(status.as_str(), "Failed" | "Cancelled" | "CancellationRequested") {
            return Ok(None);
        }
        let trigger_event_id = previous.and_then(|event| event.correlation_id.as_deref());
        let goal = trigger_event_id.and_then(|trigger| {
            events
                .iter()
                .find(|event| {
                    event.kind == JournalEventKind::IngressAccepted
                        && event.payload.get("event_id").and_then(Value::as_str) == Some(trigger)
                })
                .and_then(|event| event.payload.get("text").and_then(Value::as_str))
        });

        let mut activity = events
            .iter()
            .filter(|event| event.run_id.as_ref() == Some(run_id))
            .filter_map(|event| match event.kind {
                JournalEventKind::ToolCallIssued => event
                    .payload
                    .get("operation")
                    .and_then(Value::as_str)
                    .map(|operation| format!("tool: {operation}")),
                JournalEventKind::ReceiptReceived => {
                    let receipt_status = event
                        .payload
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown");
                    let output = event.payload.get("output").cloned().unwrap_or(Value::Null);
                    Some(format!(
                        "result {receipt_status}: {}",
                        truncate_chars(&output.to_string(), 1200)
                    ))
                }
                JournalEventKind::RunFailed | JournalEventKind::RunCancelled => Some(format!(
                    "terminal: {}",
                    truncate_chars(&event.payload.to_string(), 600)
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        let keep_from = activity.len().saturating_sub(12);
        activity.drain(0..keep_from);
        let mut lines = vec![format!(
            "Previous unfinished Run ({}), status={status}.",
            run_id.0
        )];
        if let Some(goal) = goal {
            lines.push(format!("Original user goal: {goal}"));
        }
        if !activity.is_empty() {
            lines.push("Recent durable activity:".to_string());
            lines.extend(activity.into_iter().map(|row| format!("- {row}")));
        }
        lines.push(
            "Continue from these facts and inspect the current workspace diff before repeating investigation."
                .to_string(),
        );
        Ok(Some(lines.join("\n")))
    }

    /// Collect every complete conversation turn for a session, in journal
    /// order. Context selection belongs to an external provider, so candidate
    /// construction must not apply a Kernel-owned turn limit.
    pub fn conversation_turns(
        &self,
        session_id: &SessionId,
        skip_event_id: Option<&str>,
    ) -> Result<Vec<(String, String)>> {
        self.recent_conversation_turns(session_id, usize::MAX, skip_event_id)
    }

    /// Collect recent complete conversation turns for a session.
    /// A complete turn = user message + AssistantReplyDelivered (same run_id).
    /// Returns at most `limit` complete turns, ordered by run creation.
    ///
    /// - User text from IngressAccepted → SessionReady pairing.
    /// - Assistant text from AssistantReplyDelivered events.
    /// - Paired by matching the run_id that triggered both.
    /// - Incomplete runs (no AssistantReplyDelivered) are excluded.
    /// - The current event (`skip_event_id`) is excluded.
    /// - Session-isolated: only events in the requested session.
    pub fn recent_conversation_turns(
        &self,
        session_id: &SessionId,
        limit: usize,
        skip_event_id: Option<&str>,
    ) -> Result<Vec<(String, String)>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let events = self.events()?;

        // 1. Collect user text: IngressAccepted → SessionReady in this session.
        let ingress_text: HashMap<String, String> = events
            .iter()
            .filter(|e| e.kind == JournalEventKind::IngressAccepted)
            .filter_map(|e| {
                let event_id = e.payload.get("event_id")?.as_str()?;
                if Some(event_id) == skip_event_id {
                    return None; // Skip current message.
                }
                let text = e.payload.get("text")?.as_str()?;
                Some((event_id.to_string(), text.to_string()))
            })
            .collect();

        // 2. Find run_ids for each user ingress in this session.
        //    RunStarted has correlation_id = ingress event_id.
        let mut ingress_run: HashMap<String, String> = HashMap::new();
        for e in &events {
            if e.kind != JournalEventKind::RunStarted || e.session_id.as_ref() != Some(session_id) {
                continue;
            }
            let Some(corr) = &e.correlation_id else {
                continue;
            };
            if ingress_text.contains_key(corr) {
                // correlation_id of RunStarted = the triggering event_id
                // But also need run_id. RunStarted payload.run_id?
                let run_id = e.payload.get("run_id").and_then(Value::as_str);
                if let Some(rid) = run_id {
                    ingress_run.insert(corr.clone(), rid.to_string());
                }
            }
        }

        // 3. Collect AssistantReplyDelivered in this session, keyed by run_id.
        //    Identity verification: payload fields must match event envelope.
        let mut reply_by_run: HashMap<String, (i64, String)> = HashMap::new();
        for e in &events {
            if e.kind != JournalEventKind::AssistantReplyDelivered
                || e.session_id.as_ref() != Some(session_id)
            {
                continue;
            }
            // Verify payload session_id matches event session_id.
            let Some(p_session_id) = e.payload.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            if p_session_id != &session_id.0 {
                continue; // Identity mismatch — ignore.
            }
            // Verify payload run_id matches event run_id — both must be present.
            let Some(ev_run_id) = e.run_id.as_ref() else {
                continue;
            };
            let Some(p_run_id) = e.payload.get("run_id").and_then(Value::as_str) else {
                continue;
            };
            if p_run_id != ev_run_id.0.as_str() {
                continue; // Identity mismatch — ignore.
            }
            // Verify invocation_id is non-empty.
            let Some(inv_id) = e.payload.get("invocation_id").and_then(Value::as_str) else {
                continue;
            };
            if inv_id.is_empty() {
                continue;
            }
            // Verify correlation_id is present and matches invocation_id.
            let Some(corr) = e.correlation_id.as_ref() else {
                continue; // Missing correlation_id — reject.
            };
            if corr.as_str() != inv_id {
                continue; // Identity mismatch — ignore.
            }
            let Some(text) = e.payload.get("text").and_then(Value::as_str) else {
                continue;
            };
            // Only keep the first (earliest) delivery per run to handle
            // idempotent retry.
            reply_by_run
                .entry(p_run_id.to_string())
                .or_insert((e.sequence, text.to_string()));
        }

        // 4. Build turns: pair user ingress by matching run_id.
        //    For each RunStarted in this session, look up the user text
        //    from its correlation_id (ingress event_id), then look up the
        //    assistant reply by the same run_id.
        //    Collect in chronological order (by RunStarted sequence).
        let mut run_events: Vec<(i64, String, String)> = vec![]; // (seq, user_text, run_id)
        for e in &events {
            if e.kind != JournalEventKind::RunStarted || e.session_id.as_ref() != Some(session_id) {
                continue;
            }
            let Some(corr) = &e.correlation_id else {
                continue;
            };
            let Some(user_text) = ingress_text.get(corr) else {
                continue;
            };
            let Some(run_id) = e.payload.get("run_id").and_then(Value::as_str) else {
                continue;
            };
            run_events.push((e.sequence, user_text.clone(), run_id.to_string()));
        }

        // 5. Match each run with its assistant reply, collect complete turns.
        let mut complete_turns: Vec<(String, String)> = vec![]; // (user, assistant)
        for (_seq, user_text, run_id) in &run_events {
            if let Some((_, assistant_text)) = reply_by_run.get(run_id) {
                complete_turns.push((user_text.clone(), assistant_text.clone()));
            }
        }

        // 6. Keep last `limit` complete turns.
        let start = complete_turns.len().saturating_sub(limit);
        Ok(complete_turns[start..].to_vec())
    }

    /// Return recent user messages (legacy — only user, no assistant).
    pub fn recent_user_messages(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        self.recent_user_messages_inner(session_id, limit)
    }

    /// Capability-boundary recall.
    #[doc(hidden)]
    pub(crate) fn recent_user_messages_for_capability(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        #[cfg(any(test, feature = "test-helpers"))]
        if self
            .recall_failure_for_test
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(anyhow::anyhow!("recall_query_failed"));
        }
        self.recent_user_messages_inner(session_id, limit)
    }

    fn recent_user_messages_inner(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let events = self.events()?;
        let mut ingress_text_by_event: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
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
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod continuation_context_tests {
    use super::*;
    use crate::domain::{
        AgentId, ChannelKind, EventId, PrincipalId, PrincipalSource, PrincipalSubject, Run,
        RunId, RunMode, RunPrincipal, RunStatus, SessionTarget,
    };
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn explicit_continue_can_see_previous_goal_and_recent_durable_results() -> Result<()> {
        let journal = super::super::JournalStore::in_memory()?;
        let session = journal.get_or_create_session(&SessionTarget {
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:test".into(),
        })?;
        let trigger = EventId::new();
        journal.append_event(
            JournalEventKind::IngressAccepted,
            None,
            None,
            Some("dedupe"),
            json!({"event_id": trigger.0, "text": "修复 failure feedback"}),
        )?;
        let now = Utc::now();
        let run = Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: AgentId("main".into()),
            trigger_event_id: trigger.clone(),
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
        journal.append_event(
            JournalEventKind::RunStarted,
            Some(&run.id),
            Some(&session.id),
            Some(&trigger.0),
            json!({"run_id": run.id.0, "trigger_event_id": trigger.0}),
        )?;
        journal.append_event(
            JournalEventKind::ToolCallIssued,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({"operation": "external.coding_workspace_write"}),
        )?;
        journal.append_event(
            JournalEventKind::ReceiptReceived,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({"status": "Succeeded", "output": {"relative_path": "src/fix.rs"}}),
        )?;
        journal.fail_run(&run.id)?;
        journal.append_event(
            JournalEventKind::RunFailed,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({"run_id": run.id.0, "reason": "tool_call_limit_reached"}),
        )?;

        let context = journal
            .previous_unfinished_run_context(&session.id, "event_continue")?
            .expect("previous failed Run context");
        assert!(context.contains("修复 failure feedback"));
        assert!(context.contains("external.coding_workspace_write"));
        assert!(context.contains("src/fix.rs"));
        assert!(context.contains("tool_call_limit_reached"));
        assert!(session.summarized_until_event_id.is_none());
        Ok(())
    }
}
