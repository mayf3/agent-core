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
        for _round in 0..MAX_TOOL_ROUNDS {
            let Some(tool_call) = llm.tool_call.clone() else {
                return Ok(llm);
            };
            // Execute the tool inline. Expected rejections (unknown operation,
            // forbidden Write, invalid arguments) become a structured ToolResult
            // so the model can recover. Infrastructure failures (Journal/SQLite/
            // Gateway integrity) propagate and fail the Run.
            let (result_text, result_json) =
                match self.handle_inline_tool_call(journal, gateway, run, session, &tool_call) {
                    Ok(Some(tuple)) => tuple,
                    Ok(None) => (
                        "tool call produced no result".to_string(),
                        json!({ "error": "tool call produced no result" }),
                    ),
                    Err(e) => return Err(e),
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
    /// no tool call was emitted.
    ///
    /// Expected rejections (unknown operation, forbidden Write, invalid
    /// arguments) become `Ok(Some(...))` — a stable ToolResult the model can
    /// recover from. Infrastructure failures (Journal/SQLite/Gateway integrity)
    /// propagate as `Err` and fail the Run. Sanitized messages are used in
    /// ToolResult content; raw error text is never exposed to the model or
    /// Journal.
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
            Err(_e) => {
                // Expected business rejection (unknown op, forbidden Write):
                // surface as a structured sanitized ToolResult so the model
                // can recover. Never expose raw error text or user-supplied
                // operation names.
                return Ok(Some((
                    "tool call rejected: operation is not available".to_string(),
                    json!({ "error": "tool call rejected" }),
                )));
            }
        };
        // Validate model-supplied arguments before injecting session context.
        // time.now and system.status accept no model arguments; any extra
        // field is rejected. session.recall_recent accepts only optional
        // limit (integer 1..20) and optional query (string).
        if let Err(_e) = validate_model_arguments(&intent.operation, &intent.arguments) {
            return Ok(Some((
                "tool call rejected: invalid arguments".to_string(),
                json!({ "error": "tool call rejected: invalid arguments" }),
            )));
        }
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
        // Gateway approval failure (policy rejection) is an expected business
        // outcome — surface as ToolResult so the model can recover, not as an
        // infrastructure failure.
        let approved = match gateway.approve_invocation(intent, &run, &session) {
            Ok(a) => a,
            Err(_e) => {
                return Ok(Some((
                    "tool call rejected: not permitted".to_string(),
                    json!({ "error": "tool call rejected: not permitted" }),
                )));
            }
        };
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
                crate::domain::operation::SYSTEM_STATUS => Self::execute_system_status(journal)?,
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
    /// raw event content. Infrastructure failures (Journal/SQLite) propagate
    /// as `Err` — they must fail the run, not silently claim healthy/zero.
    pub(crate) fn execute_system_status(
        journal: &JournalStore,
    ) -> Result<(crate::domain::ReceiptStatus, serde_json::Value, String)> {
        let hash_ok = journal.verify_hash_chain()?;
        let pending = journal.outbox_status_count(crate::domain::OutboxDispatchStatus::Pending)?;
        let unknown = journal.outbox_unknown_unacked_count()?;
        let drift = journal.outbox_projection_drift_count()?;
        let undelivered = journal.undelivered_ingress_events()?.len() as i64;

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

        Ok((crate::domain::ReceiptStatus::Succeeded, output, text))
    }
}

/// Validate model-supplied tool-call arguments before the Runtime injects
/// trusted session context. Rejects extra fields, wrong types, and out-of-range
/// values so malformed model output never executes a tool.
///
/// * `time.now` — no model arguments allowed.
/// * `system.status` — no model arguments allowed.
/// * `session.recall_recent` — optional `limit` (integer 1..20), optional
///   `query` (string); any other field is rejected.
fn validate_model_arguments(operation: &str, arguments: &serde_json::Value) -> anyhow::Result<()> {
    let Some(map) = arguments.as_object() else {
        anyhow::bail!("arguments must be a JSON object");
    };
    match operation {
        crate::domain::operation::TIME_NOW | crate::domain::operation::SYSTEM_STATUS => {
            if !map.is_empty() {
                anyhow::bail!("operation takes no arguments");
            }
        }
        crate::domain::operation::SESSION_RECALL_RECENT => {
            for (key, value) in map {
                match key.as_str() {
                    "limit" => {
                        let n = value
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("limit must be a positive integer"))?;
                        if n < 1 || n > 20 {
                            anyhow::bail!("limit must be between 1 and 20");
                        }
                    }
                    "query" => {
                        if !value.is_string() {
                            anyhow::bail!("query must be a string");
                        }
                    }
                    _ => anyhow::bail!("unexpected argument: {key}"),
                }
            }
        }
        _ => anyhow::bail!("unknown operation"),
    }
    Ok(())
}
