//! Generic cooperative Run cancellation facts.
//!
//! This module deliberately contains no connector command, presentation, or
//! transport semantics. A Host may request cancellation; a Runtime may only
//! observe the durable request at a safe model/tool boundary and settle it.

use super::queue::append_event_tx;
use super::sqlite::JournalStore;
use crate::domain::{JournalEventKind, RunId, SessionId};
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellationRequestOutcome {
    Requested,
    AlreadyRequested,
    AlreadyCancelled,
    NotCancellable(String),
    RunNotFound,
}

impl JournalStore {
    /// Request cooperative cancellation of a currently executing Run.
    ///
    /// The status transition and audit fact are committed atomically. Only a
    /// genuinely executing `Running` Run is cancellable in V0; a reply already
    /// waiting for dispatch is beyond the model/tool cancellation boundary.
    pub fn request_run_cancellation(
        &self,
        run_id: &RunId,
        requested_by: &str,
    ) -> Result<CancellationRequestOutcome> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT status, session_id FROM runs WHERE id = ?1",
                params![run_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((status, session_id)) = row else {
            tx.commit()?;
            return Ok(CancellationRequestOutcome::RunNotFound);
        };
        match status.as_str() {
            "CancellationRequested" => {
                tx.commit()?;
                return Ok(CancellationRequestOutcome::AlreadyRequested);
            }
            "Cancelled" => {
                tx.commit()?;
                return Ok(CancellationRequestOutcome::AlreadyCancelled);
            }
            "Running" => {}
            other => {
                tx.commit()?;
                return Ok(CancellationRequestOutcome::NotCancellable(
                    other.to_string(),
                ));
            }
        }
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE runs SET status = 'CancellationRequested', updated_at = ?1
             WHERE id = ?2 AND status = 'Running'",
            params![now, run_id.0],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(CancellationRequestOutcome::NotCancellable(
                "concurrent_terminal_transition".to_string(),
            ));
        }
        let session_id = SessionId(session_id);
        append_event_tx(
            &tx,
            JournalEventKind::RunCancellationRequested,
            Some(run_id),
            Some(&session_id),
            Some(&format!("cancel:{}", run_id.0)),
            json!({
                "run_id": run_id.0,
                "requested_by": requested_by,
                "requested_at": now,
            }),
        )?;
        tx.commit()?;
        Ok(CancellationRequestOutcome::Requested)
    }

    pub fn run_cancellation_requested(&self, run_id: &RunId) -> Result<bool> {
        Ok(self.run_status(run_id)?.as_deref() == Some("CancellationRequested"))
    }

    /// Settle a previously requested cancellation atomically with its terminal
    /// fact. Returns false when no request is pending (including an already
    /// terminal Run), making the boundary check idempotent.
    pub fn finish_run_cancelled(&self, run_id: &RunId, reason: &str) -> Result<bool> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let session_id: Option<String> = tx
            .query_row(
                "SELECT session_id FROM runs
                 WHERE id = ?1 AND status = 'CancellationRequested'",
                params![run_id.0],
                |row| row.get(0),
            )
            .optional()?;
        let Some(session_id) = session_id else {
            tx.commit()?;
            return Ok(false);
        };
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE runs SET status = 'Cancelled', updated_at = ?1
             WHERE id = ?2 AND status = 'CancellationRequested'",
            params![now, run_id.0],
        )?;
        let session_id = SessionId(session_id);
        append_event_tx(
            &tx,
            JournalEventKind::RunCancelled,
            Some(run_id),
            Some(&session_id),
            Some(&format!("cancel:{}", run_id.0)),
            json!({
                "run_id": run_id.0,
                "reason": reason,
                "cancelled_at": now,
            }),
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentId, ChannelKind, EventId, PrincipalId, PrincipalSource, PrincipalSubject, Run,
        RunMode, RunPrincipal, RunStatus, SessionTarget,
    };

    fn running_run(journal: &JournalStore) -> Result<Run> {
        let session = journal.get_or_create_session(&SessionTarget {
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "cancel-test".into(),
        })?;
        let now = Utc::now();
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
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
    fn cancellation_request_and_terminal_fact_are_idempotent() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let run = running_run(&journal)?;
        assert_eq!(
            journal.request_run_cancellation(&run.id, "test-host")?,
            CancellationRequestOutcome::Requested
        );
        assert!(journal.run_cancellation_requested(&run.id)?);
        assert_eq!(
            journal.request_run_cancellation(&run.id, "test-host")?,
            CancellationRequestOutcome::AlreadyRequested
        );
        assert!(journal.finish_run_cancelled(&run.id, "user_requested")?);
        assert!(!journal.finish_run_cancelled(&run.id, "user_requested")?);
        assert_eq!(journal.run_status(&run.id)?.as_deref(), Some("Cancelled"));
        let events = journal.events()?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == JournalEventKind::RunCancellationRequested)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == JournalEventKind::RunCancelled)
                .count(),
            1
        );
        assert!(journal.verify_hash_chain()?);
        Ok(())
    }
}
