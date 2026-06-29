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
            // Verify correlation_id matches payload invocation_id (production
            // contract: event.correlation_id == payload.invocation_id).
            if let Some(corr) = e.correlation_id.as_ref() {
                if corr.as_str() != inv_id {
                    continue; // Identity mismatch — ignore.
                }
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
