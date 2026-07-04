use super::queue::append_event_tx;
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::json;
const TERMINAL_TRANSITION_ERROR: &str = "outbox_dispatch_terminal_transition_not_allowed";
impl JournalStore {
    pub fn queue_outbox_dispatch(
        &self,
        approved: &ApprovedInvocation,
        session_id: Option<&SessionId>,
    ) -> Result<String> {
        let intent = approved.intent();
        let dispatch_id = format!("dispatch:{}", intent.invocation_id.0);
        let idempotency_key = intent
            .idempotency_key
            .clone()
            .unwrap_or_else(|| intent.invocation_id.0.clone());
        let arguments_json = serde_json::to_string(&intent.arguments)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO outbox_dispatches
             (dispatch_id, invocation_id, run_id, session_id, operation, arguments_json,
              idempotency_key, decision_id, status, attempts, available_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?10, 0, ?9, ?9, ?9)",
            params![
                dispatch_id.as_str(),
                intent.invocation_id.0.as_str(),
                intent.run_id.0.as_str(),
                session_id.map(|id| id.0.as_str()),
                intent.operation.as_str(),
                arguments_json.as_str(),
                idempotency_key.as_str(),
                approved.decision_id.as_str(),
                now.as_str(),
                OutboxDispatchStatus::Pending.as_str(),
            ],
        )?;
        if changed == 1 {
            append_event_tx(
                &tx,
                JournalEventKind::OutboxQueued,
                Some(&intent.run_id),
                session_id,
                Some(&intent.invocation_id.0),
                json!({
                    "dispatch_id": dispatch_id,
                    "invocation_id": intent.invocation_id.0.as_str(),
                    "decision_id": approved.decision_id.as_str(),
                    "operation": intent.operation.as_str(),
                    "idempotency_key": idempotency_key,
                    "status": OutboxDispatchStatus::Pending.as_str(),
                }),
            )?;
        }
        tx.commit()?;
        Ok(dispatch_id)
    }
    pub fn outbox_dispatch_status(
        &self,
        invocation_id: &InvocationId,
    ) -> Result<Option<OutboxDispatchStatus>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM outbox_dispatches WHERE invocation_id = ?1",
                params![invocation_id.0],
                |row| row.get(0),
            )
            .optional()?;
        Ok(status.and_then(|s| OutboxDispatchStatus::parse_opt(&s)))
    }
    pub fn start_outbox_dispatch(
        &self,
        approved: &ApprovedInvocation,
        session_id: Option<&SessionId>,
    ) -> Result<()> {
        let intent = approved.intent();
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE outbox_dispatches
             SET status = ?1, attempts = attempts + 1, updated_at = ?2
             WHERE invocation_id = ?3 AND status = ?4",
            params![
                OutboxDispatchStatus::Dispatching.as_str(),
                now.as_str(),
                intent.invocation_id.0.as_str(),
                OutboxDispatchStatus::Pending.as_str(),
            ],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Err(anyhow!("outbox_dispatch_not_startable"));
        }
        append_event_tx(
            &tx,
            JournalEventKind::DispatchStarted,
            Some(&intent.run_id),
            session_id,
            Some(&intent.invocation_id.0),
            json!({ "operation": intent.operation.as_str() }),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn succeed_outbox_dispatch(
        &self,
        receipt: &Receipt,
        run_id: &RunId,
        session_id: Option<&SessionId>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE outbox_dispatches
             SET status = ?1, updated_at = ?2
             WHERE invocation_id = ?3 AND status = ?4",
            params![
                OutboxDispatchStatus::Succeeded.as_str(),
                now.as_str(),
                receipt.invocation_id.0.as_str(),
                OutboxDispatchStatus::Dispatching.as_str(),
            ],
        )?;
        if changed == 0 {
            bail!(TERMINAL_TRANSITION_ERROR);
        }
        // ReceiptReceived — only safe fields, never raw connector output.
        append_event_tx(
            &tx,
            JournalEventKind::ReceiptReceived,
            Some(run_id),
            session_id,
            Some(&receipt.invocation_id.0),
            json!({
                "status": format!("{:?}", receipt.status),
                "external_ref": receipt.external_ref,
                "output_kind": "text",
            }),
        )?;
        // AssistantReplyDelivered — only for reply operations (stdout/feishu)
        // with a Succeeded receipt and known delivery text.
        if receipt.status == ReceiptStatus::Succeeded {
            if let Some(ref session) = session_id {
                self.maybe_record_assistant_reply(&tx, &receipt, run_id, session)?;
            }
        }
        // Don't transition Failed runs to Completed (e.g. failure-reply outbox).
        let current_status: Option<String> = tx
            .query_row(
                "SELECT status FROM runs WHERE id = ?1",
                params![run_id.0.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let is_failed = matches!(current_status.as_deref(), Some("Failed"));
        if !is_failed {
            tx.execute(
                "UPDATE runs SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params!["Completed", now.as_str(), run_id.0.as_str()],
            )?;
            append_event_tx(
                &tx,
                JournalEventKind::RunCompleted,
                Some(run_id),
                session_id,
                Some(&receipt.invocation_id.0),
                json!({
                    "status": "Completed",
                    "reason": "outbox_dispatch_succeeded",
                }),
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Write AssistantReplyDelivered if the invocation is a reply operation
    /// (stdout.send_text, feishu.send_message) with a known text argument.
    /// Uses a UNIQUE constraint on invocation_id for idempotency.
    fn maybe_record_assistant_reply(
        &self,
        tx: &rusqlite::Transaction<'_>,
        receipt: &Receipt,
        run_id: &RunId,
        session_id: &SessionId,
    ) -> Result<()> {
        let inv_id = &receipt.invocation_id.0;

        // Load the outbox row to get operation and arguments.
        let row: Result<(String, String), _> = tx.query_row(
            "SELECT operation, arguments_json FROM outbox_dispatches WHERE invocation_id = ?1",
            params![inv_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        let (operation, arguments_json) = match row {
            Ok(r) => r,
            Err(_) => return Ok(()), // No outbox row — not a reply.
        };

        // Only reply operations have a text argument to record.
        let is_reply = operation == "stdout.send_text" || operation == "feishu.send_message";
        if !is_reply {
            return Ok(());
        }

        // Extract the user-visible text from arguments.
        let arguments: serde_json::Value = match serde_json::from_str(&arguments_json) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        let text = match arguments.get("text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return Ok(()),
        };

        // Determine the channel from the operation name.
        let channel = match operation.as_str() {
            "stdout.send_text" => "cli",
            "feishu.send_message" => "feishu",
            _ => return Ok(()),
        };

        // Write AssistantReplyDelivered with UNIQUE idempotency.
        let payload = serde_json::json!({
            "session_id": session_id.0,
            "run_id": run_id.0,
            "invocation_id": inv_id,
            "channel": channel,
            "text": text,
        });
        let result = append_event_tx(
            tx,
            JournalEventKind::AssistantReplyDelivered,
            Some(run_id),
            Some(session_id),
            Some(inv_id),
            payload,
        );
        if let Err(e) = result {
            // If duplicate (UNIQUE on kind+sequence? No — journal_events
            // has no unique constraint on invocation). Use the row count
            // of a prior existence check instead.
            if !self.assistant_reply_exists(tx, inv_id)? {
                return Err(anyhow!("failed to record assistant reply: {e}"));
            }
        }
        Ok(())
    }

    /// Check if an AssistantReplyDelivered event already exists for this
    /// invocation.
    fn assistant_reply_exists(
        &self,
        tx: &rusqlite::Transaction<'_>,
        invocation_id: &str,
    ) -> Result<bool> {
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM journal_events
             WHERE kind = 'AssistantReplyDelivered'
             AND correlation_id = ?1",
            params![invocation_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn fail_outbox_dispatch(
        &self,
        invocation_id: &InvocationId,
        run_id: &RunId,
        session_id: Option<&SessionId>,
        error: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE outbox_dispatches
             SET status = ?1, locked_by = NULL, locked_until = NULL,
                 last_error = ?2, updated_at = ?3
             WHERE invocation_id = ?4 AND status = ?5",
            params![
                OutboxDispatchStatus::Failed.as_str(),
                error,
                now.as_str(),
                invocation_id.0.as_str(),
                OutboxDispatchStatus::Dispatching.as_str(),
            ],
        )?;
        if changed == 0 {
            bail!(TERMINAL_TRANSITION_ERROR);
        }
        append_event_tx(
            &tx,
            JournalEventKind::ReceiptReceived,
            Some(run_id),
            session_id,
            Some(&invocation_id.0),
            json!({
                "status": "Failed",
                "error": error,
                "output_kind": "error",
            }),
        )?;
        tx.execute(
            "UPDATE runs SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params!["Failed", now.as_str(), run_id.0.as_str()],
        )?;
        append_event_tx(
            &tx,
            JournalEventKind::RunFailed,
            Some(run_id),
            session_id,
            Some(&invocation_id.0),
            json!({
                "status": "Failed",
                "reason": "outbox_dispatch_definite_failure",
                "error": error,
            }),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn unknown_outbox_dispatch(
        &self,
        invocation_id: &InvocationId,
        run_id: &RunId,
        session_id: Option<&SessionId>,
        error: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE outbox_dispatches
             SET status = ?1, locked_by = NULL, locked_until = NULL,
                 last_error = ?2, updated_at = ?3
             WHERE invocation_id = ?4 AND status = ?5",
            params![
                OutboxDispatchStatus::Unknown.as_str(),
                error,
                now.as_str(),
                invocation_id.0.as_str(),
                OutboxDispatchStatus::Dispatching.as_str(),
            ],
        )?;
        if changed == 0 {
            bail!(TERMINAL_TRANSITION_ERROR);
        }
        append_event_tx(
            &tx,
            JournalEventKind::OutboxDispatchUnknown,
            Some(run_id),
            session_id,
            Some(&invocation_id.0),
            json!({
                "invocation_id": invocation_id.0.as_str(),
                "error": error,
                "status": OutboxDispatchStatus::Unknown.as_str(),
            }),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_outbox_retryable_failed(
        &self,
        invocation_id: &InvocationId,
        run_id: &RunId,
        session_id: Option<&SessionId>,
        error: &str,
        policy: &RetryPolicy,
    ) -> Result<()> {
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let attempts: i64 = tx
            .query_row(
                "SELECT attempts FROM outbox_dispatches WHERE invocation_id = ?1",
                params![invocation_id.0],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if attempts >= policy.max_outbox_attempts {
            drop(tx);
            drop(conn);
            return self.mark_outbox_dead(invocation_id, run_id, session_id, error, policy);
        }
        let delay_ms = next_retry_delay_ms(
            attempts + 1,
            policy.base_retry_delay_ms,
            policy.max_retry_delay_ms,
        );
        let available_at = now + chrono::Duration::milliseconds(delay_ms);
        let available_at_text = available_at.to_rfc3339();
        append_event_tx(
            &tx,
            JournalEventKind::OutboxDispatchFailed,
            Some(run_id),
            session_id,
            Some(&invocation_id.0),
            json!({
                "invocation_id": invocation_id.0.as_str(),
                "reason_category": error,
                "retryable": true,
                "next_available_at": available_at_text,
                "attempts": attempts,
            }),
        )?;
        tx.execute(
            "UPDATE outbox_dispatches
             SET status = ?1, last_error = ?2,
                 locked_by = NULL, locked_until = NULL,
                 available_at = ?3, updated_at = ?4
             WHERE invocation_id = ?5",
            params![
                OutboxDispatchStatus::RetryableFailed.as_str(),
                error,
                available_at_text,
                now_text,
                invocation_id.0,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn mark_outbox_dead(
        &self,
        invocation_id: &InvocationId,
        run_id: &RunId,
        session_id: Option<&SessionId>,
        error: &str,
        _policy: &RetryPolicy,
    ) -> Result<()> {
        let now_text = Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        append_event_tx(
            &tx,
            JournalEventKind::OutboxDispatchDead,
            Some(run_id),
            session_id,
            Some(&invocation_id.0),
            json!({
                "invocation_id": invocation_id.0.as_str(),
                "reason_category": error,
                "retryable": false,
            }),
        )?;
        tx.execute(
            "UPDATE outbox_dispatches
             SET status = ?1, last_error = ?2,
                 locked_by = NULL, locked_until = NULL, updated_at = ?3
             WHERE invocation_id = ?4",
            params![
                OutboxDispatchStatus::Dead.as_str(),
                error,
                now_text,
                invocation_id.0,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}
