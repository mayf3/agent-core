use super::sqlite::JournalStore;
use crate::domain::{JournalEvent, JournalEventKind, RunId};
use anyhow::Result;
use std::collections::HashSet;

impl JournalStore {
    pub fn ingress_event_by_event_id(&self, event_id: &str) -> Result<Option<JournalEvent>> {
        Ok(self.events()?.into_iter().find(|event| {
            event.kind == JournalEventKind::IngressAccepted
                && event
                    .payload
                    .get("event_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(event_id)
        }))
    }

    pub fn undelivered_ingress_events(&self) -> Result<Vec<JournalEvent>> {
        let events = self.events()?;
        let mut delivered = HashSet::new();
        for event in &events {
            // An ingress event is considered "delivered" once the worker has
            // started, completed, or failed processing it. `RunFailed` is
            // included on purpose: a failed worker delivery must NOT be
            // re-queued on restart (see
            // docs/decisions/worker-failure-journal-kind.md). Excluding it
            // here would re-queue failed ingress forever.
            if matches!(
                event.kind,
                JournalEventKind::SessionReady
                    | JournalEventKind::RunStarted
                    | JournalEventKind::RunCompleted
                    | JournalEventKind::RunFailed
            ) {
                if let Some(correlation_id) = &event.correlation_id {
                    delivered.insert(correlation_id.clone());
                }
            }
        }
        Ok(events
            .into_iter()
            .filter(|event| event.kind == JournalEventKind::IngressAccepted)
            .filter(|event| {
                event
                    .payload
                    .get("event_id")
                    .and_then(serde_json::Value::as_str)
                    .map(|event_id| !delivered.contains(event_id))
                    .unwrap_or(false)
            })
            .collect())
    }

    /// Phase 2 M2d: return the `ApprovalRequested` payload for a run, if any.
    /// Scans the run's journal events for the most recent `ApprovalRequested`
    /// fact (a run may be paused at most once at a time). Returns `None` if the
    /// run was never paused. The payload carries the full `intent_snapshot`
    /// (operation, arguments, idempotency_key, decision_id, run_id, session_id)
    /// so `Gateway::approve_run` can reconstruct and queue the dispatch.
    pub fn approval_request_for_run(&self, run_id: &RunId) -> Result<Option<serde_json::Value>> {
        let mut found = None;
        for event in self.events()? {
            if event.kind == JournalEventKind::ApprovalRequested
                && event.run_id.as_ref() == Some(run_id)
            {
                found = Some(event.payload);
            }
        }
        Ok(found)
    }
}
