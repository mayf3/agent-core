//! Tool-recall loop for the Runtime (Phase 2 tool-call execution).
//!
//! When the model emits a read-only tool call, the Runtime executes it inline,
//! appends the result as a `ToolResult` context block, and re-invokes the LLM
//! so it can fold the tool output into its reply. This is a **bounded
//! read-only loop** (max [`MAX_TOOL_ROUNDS`]), NOT a general workflow engine.
//!
//! See `docs/decisions/tool-call-execution-loop.md`.

use crate::adapters::InvocationAdapter;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput};
use anyhow::Result;
use serde_json::json;

/// Maximum number of tool-execution rounds within a single run. Round 1 = model
/// proposes a read-only tool; the Runtime executes it inline, appends a
/// `ToolResult` block, and re-invokes the LLM. Round 2 = model folds the tool
/// output into its reply. This cap stops a runaway model from looping forever.
pub(crate) const MAX_TOOL_ROUNDS: usize = 2;

impl<L: LlmClient> super::Runtime<L> {
    /// Run the tool-recall loop: when the model emits a read-only tool call,
    /// execute it inline, append the result as a `ToolResult` context block, and
    /// re-invoke the LLM so it can fold the tool output into its reply. Repeats
    /// up to [`MAX_TOOL_ROUNDS`] times and returns the final `LlmOutput`.
    ///
    /// Backwards compatible: with no tool call this returns the original output
    /// unchanged and leaves `blocks` untouched. Tool failures are not fatal —
    /// the error is appended as a `ToolResult` so the model can recover.
    pub(crate) fn run_tool_recall_loop(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        blocks: &mut Vec<ContextBlock>,
        user_text: &str,
        mut llm: LlmOutput,
    ) -> Result<LlmOutput> {
        for round in 0..MAX_TOOL_ROUNDS {
            let Some(tool_call) = llm.tool_call.clone() else {
                return Ok(llm);
            };
            // Execute the tool inline. Errors become a structured ToolResult
            // (error note) rather than aborting the run.
            let (result_text, result_json) =
                match self.handle_inline_tool_call(journal, gateway, run, session, &tool_call) {
                    Ok(Some(tuple)) => tuple,
                    Ok(None) => (
                        "tool call produced no result".to_string(),
                        json!({ "error": "tool call produced no result" }),
                    ),
                    Err(e) => (
                        format!("tool call failed: {}", e),
                        json!({ "error": format!("tool call failed: {}", e) }),
                    ),
                };
            // Append the result as a context block the next round sees. We embed
            // both the summary and the structured JSON so the model can fold a
            // recall's messages (or a failure's `error` key) into its reply.
            blocks.push(ContextBlock {
                kind: ContextBlockKind::ToolResult,
                content: format!(
                    "tool: {}\nresult: {}\noutput: {}",
                    tool_call.operation, result_text, result_json,
                ),
                compressibility: Compressibility::Summarizable,
                source_ref: Some(format!("tool:{}", tool_call.operation)),
            });
            let next = self.llm.complete(LlmInput {
                blocks: blocks.clone(),
                user_text: user_text.to_string(),
            })?;
            journal.append_event(
                JournalEventKind::LlmCompleted,
                Some(&run.id),
                Some(&session.id),
                None,
                next.journal_payload.clone(),
            )?;
            llm = next;
            // If the model stopped proposing tools, we're done.
            if llm.tool_call.is_none() {
                return Ok(llm);
            }
        }
        // Hit MAX_TOOL_ROUNDS: use the last round's output as the reply, but
        // ensure the final reply explicitly tells the user the tool-call limit
        // was reached.
        if llm.tool_call.is_some() {
            llm.content = format!(
                "{}\n\n[Reached tool-call limit ({MAX_TOOL_ROUNDS}). Using the best answer from the last round.]",
                if llm.content.is_empty() {
                    "I gathered information but couldn't finish within the tool-call limit."
                } else {
                    &llm.content
                }
            );
        }
        Ok(llm)
    }

    /// Phase 2 tool-call MVP: validate a model-emitted `ReadOnly` tool call,
    /// execute it inline, and journal the receipt (InvocationProposed →
    /// InvocationApproved → ReceiptReceived) WITHOUT queueing into
    /// `outbox_dispatches`. Returns `(result_text, result_json)`, or `None` if
    /// no tool call was emitted / rejected.
    pub(crate) fn handle_inline_tool_call(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        tool_call: &crate::llm::ToolCall,
    ) -> Result<Option<(String, serde_json::Value)>> {
        let mut intent = match crate::gateway::validate_tool_call(tool_call, &run.id) {
            Ok(intent) => intent,
            Err(e) => {
                // Rejection is surfaced as a structured failure so a follow-up
                // round can explain the rejection to the model — never a crash.
                return Ok(Some((
                    format!("tool call rejected: {}", e),
                    json!({ "error": format!("tool call rejected: {}", e) }),
                )));
            }
        };
        if let Some(arguments) = intent.arguments.as_object_mut() {
            arguments.insert("session_id".to_string(), json!(session.id.0));
        }
        let correlation_id = intent.invocation_id.0.clone();
        journal.append_event(
            JournalEventKind::InvocationProposed,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "operation": intent.operation,
                "idempotency_key": intent.idempotency_key,
                "source": "model_tool_call",
            }),
        )?;
        let approved = gateway.approve_invocation(intent, &run, &session)?;
        journal.append_event(
            JournalEventKind::InvocationApproved,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "decision_id": approved.decision_id,
                "operation": approved.intent().operation,
            }),
        )?;
        // Route to the correct inline executor by operation name.
        let (receipt_status, receipt_output, result_text) =
            match approved.intent().operation.as_str() {
                crate::domain::operation::TIME_NOW => {
                    let receipt = crate::adapters::TimeAdapter.execute(&approved)?;
                    let text = receipt
                        .output
                        .get("iso")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ok")
                        .to_string();
                    (receipt.status, receipt.output, text)
                }
                crate::domain::operation::SESSION_RECALL_RECENT => {
                    let (status, output, text) =
                        Self::execute_session_recall(journal, &session.id, &approved);
                    (status, output, text)
                }
                crate::domain::operation::SYSTEM_STATUS => {
                    let (status, output, text) = Self::execute_system_status(journal);
                    (status, output, text)
                }
                other => (
                    crate::domain::ReceiptStatus::Failed,
                    json!({ "error": format!("inline execution not implemented for {other}") }),
                    format!("tool not implemented: {other}"),
                ),
            };
        journal.append_event(
            JournalEventKind::ReceiptReceived,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "invocation_id": approved.intent().invocation_id,
                "status": format!("{:?}", receipt_status),
                "output": receipt_output.clone(),
            }),
        )?;
        Ok(Some((result_text, receipt_output)))
    }

    /// Execute `session.recall_recent`: read recent user messages from the
    /// **current session only** and return a normalized result (event_id + role
    /// + text, truncated to 500 chars/msg). No raw payload, cross-session,
    /// filesystem, or network.
    pub(crate) fn execute_session_recall(
        journal: &JournalStore,
        session_id: &SessionId,
        approved: &ApprovedInvocation,
    ) -> (crate::domain::ReceiptStatus, serde_json::Value, String) {
        const MAX_RECALL_LIMIT: usize = 20;
        const MAX_RECALL_CHARS: usize = 500;

        let args = &approved.intent().arguments;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, MAX_RECALL_LIMIT as u64) as usize)
            .unwrap_or(5);
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        let messages = match journal.recent_user_messages(session_id, limit) {
            Ok(msgs) => msgs,
            Err(_) => {
                return (
                    crate::domain::ReceiptStatus::Failed,
                    json!({ "error": "failed to read session history" }),
                    "session recall failed".to_string(),
                );
            }
        };

        let mut results: Vec<serde_json::Value> = Vec::new();
        for (event_id, text) in &messages {
            if let Some(ref q) = query {
                if !text.to_lowercase().contains(q) {
                    continue;
                }
            }
            let truncated: String = text.chars().take(MAX_RECALL_CHARS).collect();
            results.push(json!({
                "event_id": event_id,
                "role": "user",
                "text": truncated,
            }));
        }

        let output = json!({
            "session_id": session_id.0,
            "count": results.len(),
            "messages": results,
        });

        let text = if results.is_empty() {
            "no matching messages found".to_string()
        } else {
            results
                .iter()
                .filter_map(|m| m.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" | ")
        };

        (crate::domain::ReceiptStatus::Succeeded, output, text)
    }

    /// Execute `system.status`: return a deterministic health/projection summary
    /// from the Journal. Only aggregate counts — never secrets, payloads, or
    /// raw event content.
    pub(crate) fn execute_system_status(
        journal: &JournalStore,
    ) -> (crate::domain::ReceiptStatus, serde_json::Value, String) {
        let hash_ok = journal.verify_hash_chain().unwrap_or(false);
        let pending = journal
            .outbox_status_count(crate::domain::OutboxDispatchStatus::Pending)
            .unwrap_or(0);
        let unknown = journal.outbox_unknown_unacked_count().unwrap_or(0);
        let drift = journal.outbox_projection_drift_count().unwrap_or(0);
        let undelivered = journal
            .undelivered_ingress_events()
            .map(|v| v.len() as i64)
            .unwrap_or(0);

        let rollup = if !hash_ok {
            "corrupt"
        } else if unknown > 0 || drift > 0 || undelivered > 0 {
            "degraded"
        } else {
            "ok"
        };

        let output = json!({
            "rollup": rollup,
            "hash_chain": if hash_ok { "intact" } else { "broken" },
            "outbox_pending": pending,
            "outbox_unknown_unacked": unknown,
            "projection_drift": drift,
            "undelivered_ingress": undelivered,
        });

        let text = format!(
            "Status: {} (hash {}, pending {}, unknown {}, drift {}, undelivered {})",
            rollup,
            if hash_ok { "intact" } else { "broken" },
            pending,
            unknown,
            drift,
            undelivered,
        );

        (crate::domain::ReceiptStatus::Succeeded, output, text)
    }
}
