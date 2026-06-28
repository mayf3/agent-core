//! Session conversation turn history for context assembly.
//!
//! ``recent_conversation_turns`` returns user/assistant pairs for past runs
//! in the same session. ``recent_user_messages`` (legacy) returns only user
//! messages and is used by the ``session.recall_recent`` capability.

use crate::domain::{JournalEventKind, SessionId};
use anyhow::Result;
use serde_json::Value;

impl super::JournalStore {
    /// Collect recent conversation turns (user + assistant text) for a session.
    /// Orders by event sequence, pairs user messages with their corresponding
    /// assistant reply. Returns at most ``limit`` complete turns.
    ///
    /// - Assistant text comes from the final successfully sent reply
    ///   (``output.text`` in ``ReceiptReceived`` with correlation_id starting
    ///   with ``"reply:"``).
    /// - Turns where the assistant has not yet replied are omitted.
    /// - The current user's own message is excluded via ``skip_event_id``.
    /// - Session-isolated: only events matching ``session_id``.
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

        // 1. Collect user messages from IngressAccepted (excluding current).
        let mut ingress_text_by_event: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for event in &events {
            if event.kind != JournalEventKind::IngressAccepted {
                continue;
            }
            let Some(event_id) = event.payload.get("event_id").and_then(Value::as_str) else {
                continue;
            };
            if Some(event_id) == skip_event_id {
                continue;
            }
            let Some(text) = event.payload.get("text").and_then(Value::as_str) else {
                continue;
            };
            ingress_text_by_event.insert(event_id.to_string(), text.to_string());
        }

        // 2. Collect user messages for this session (in sequence order).
        let mut user_events: Vec<(i64, String)> = vec![];
        for event in &events {
            if event.kind != JournalEventKind::SessionReady
                || event.session_id.as_ref() != Some(session_id)
            {
                continue;
            }
            let Some(ingress_event_id) = &event.correlation_id else {
                continue;
            };
            if skip_event_id.map_or(false, |skip| ingress_event_id == skip) {
                continue;
            }
            let Some(text) = ingress_text_by_event.get(ingress_event_id) else {
                continue;
            };
            user_events.push((event.sequence, text.clone()));
        }

        // 3. Collect assistant reply texts from ReceiptReceived for reply ops.
        let mut reply_texts: Vec<(i64, String)> = vec![];
        for event in &events {
            if event.kind != JournalEventKind::ReceiptReceived
                || event.session_id.as_ref() != Some(session_id)
            {
                continue;
            }
            let Some(corr) = &event.correlation_id else {
                continue;
            };
            if !corr.starts_with("reply:") {
                continue;
            }
            let Some(text) = event
                .payload
                .get("output")
                .and_then(|o| o.get("text"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            reply_texts.push((event.sequence, text.to_string()));
        }

        user_events.sort_by_key(|(seq, _)| *seq);
        reply_texts.sort_by_key(|(seq, _)| *seq);

        // 4. Interleave by sequence order.
        let mut turns: Vec<(String, String)> = vec![];
        let mut reply_idx = 0;
        for (user_seq, user_text) in &user_events {
            while reply_idx < reply_texts.len() && reply_texts[reply_idx].0 < *user_seq {
                reply_idx += 1;
            }
            if reply_idx < reply_texts.len() {
                turns.push((user_text.clone(), reply_texts[reply_idx].1.clone()));
                reply_idx += 1;
            } else {
                // No reply yet — include user message without assistant turn.
                turns.push((user_text.clone(), String::new()));
            }
        }

        let start = turns.len().saturating_sub(limit);
        Ok(turns[start..].to_vec())
    }

    /// Return recent user messages (legacy — only user, no assistant). Used by
    /// the ``session.recall_recent`` capability.
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

    /// Capability-boundary recall: identical to ``recent_user_messages`` but
    /// honors the test-only deterministic fault flag.
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
        let mut ingress_text_by_event = std::collections::HashMap::new();
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
