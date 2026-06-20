use crate::adapters::InvocationAdapter;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCallResult};
use anyhow::Result;
use serde_json::json;

pub(crate) const MAX_TOOL_ROUNDS: usize = 2;

impl<L: LlmClient> super::Runtime<L> {
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
            match llm.tool_call.clone() {
                ToolCallResult::Absent => return Ok(llm),
                ToolCallResult::Malformed(reason) => {
                    blocks.push(ContextBlock {
                        kind: ContextBlockKind::ToolResult,
                        content: format!("tool call malformed: {reason}"),
                        compressibility: Compressibility::Summarizable,
                        source_ref: None,
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
                    if llm.tool_call.is_absent() {
                        return Ok(llm);
                    }
                    // If still present (valid or malformed), loop again.
                    continue;
                }
                ToolCallResult::Valid(tool_call) => {
                    let (result_text, result_json) = match self
                        .handle_inline_tool_call(journal, gateway, run, session, &tool_call)
                    {
                        Ok(Some(tuple)) => tuple,
                        Ok(None) => (
                            "tool call produced no result".to_string(),
                            json!({ "error": "tool call produced no result" }),
                        ),
                        Err(e) => return Err(e),
                    };
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
                    if llm.tool_call.is_absent() {
                        return Ok(llm);
                    }
                }
            }
        }
        if !llm.tool_call.is_absent() {
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

    pub(crate) fn handle_inline_tool_call(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        tool_call: &crate::llm::ToolCall,
    ) -> Result<Option<(String, serde_json::Value)>> {
        // Always write ToolCallIssued when processing a tool call — whether
        // the call is later accepted or rejected. This gives a complete
        // audit trail at the point of encounter.
        journal.append_event(
            JournalEventKind::ToolCallIssued,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({
                "operation": tool_call.operation,
                "tool_call_id": tool_call.id,
            }),
        )?;
        let mut intent = match crate::gateway::validate_tool_call(tool_call, &run.id) {
            Ok(intent) => intent,
            Err(e) => {
                // Tool call rejected before InvocationProposed. Write a
                // ToolCallRejected fact (NOT ReceiptReceived — the invocation
                // was never approved or executed).
                let (category, _) = sanitize_rejection(&e);
                journal.append_event(
                    JournalEventKind::ToolCallRejected,
                    Some(&run.id),
                    Some(&session.id),
                    None,
                    json!({
                        "operation": tool_call.operation,
                        "tool_call_id": tool_call.id,
                        "error_category": category,
                    }),
                )?;
                return Ok(Some((
                    format!("tool call rejected: {}", category),
                    json!({ "error": category }),
                )));
            }
        };
        if let Err(e) = validate_model_arguments(&intent.operation, &intent.arguments) {
            let (category, _) = sanitize_rejection(&e);
            journal.append_event(
                JournalEventKind::ToolCallRejected,
                Some(&run.id),
                Some(&session.id),
                None,
                json!({
                    "operation": intent.operation,
                    "tool_call_id": tool_call.id,
                    "error_category": category,
                }),
            )?;
            return Ok(Some((
                format!("tool call rejected: {}", category),
                json!({ "error": category }),
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
        let approved = match gateway.approve_invocation(intent, &run, &session) {
            Ok(a) => a,
            Err(_e) => {
                // Gateway denial: write ToolCallRejected (not ReceiptReceived —
                // the capability was never executed).
                journal.append_event(
                    JournalEventKind::ToolCallRejected,
                    Some(&run.id),
                    Some(&session.id),
                    Some(&correlation_id),
                    json!({
                        "operation": "tool_call",
                        "invocation_id": correlation_id,
                        "error_category": "policy_denied",
                    }),
                )?;
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
                    Self::execute_session_recall(journal, &session.id, &approved)?
                }
                crate::domain::operation::SYSTEM_STATUS => {
                    let output = crate::capabilities::execute(journal)?;
                    let text = output
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ok")
                        .to_string();
                    (crate::domain::ReceiptStatus::Succeeded, output, text)
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

    pub(crate) fn execute_session_recall(
        journal: &JournalStore,
        session_id: &SessionId,
        approved: &ApprovedInvocation,
    ) -> Result<(crate::domain::ReceiptStatus, serde_json::Value, String)> {
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

        let messages = journal.recent_user_messages(session_id, limit)?;

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

        Ok((crate::domain::ReceiptStatus::Succeeded, output, text))
    }
}

pub fn validate_model_arguments(
    operation: &str,
    arguments: &serde_json::Value,
) -> anyhow::Result<()> {
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

/// Sanitize a rejection error into a (category, safe_message) pair.
///
/// The category is a fixed enum-like string; the message is a fixed
/// security-safe string (never exposes raw anyhow internals, SQL, paths,
/// tokens, keys, or credentials). The caller formats the final user-facing
/// rejection text from the category, never from the raw error.
pub(crate) fn sanitize_rejection(e: &anyhow::Error) -> (&'static str, &'static str) {
    let msg = e.to_string();
    if msg.contains("unknown_operation") {
        (
            "unknown_operation",
            "Tool call rejected: unknown operation.",
        )
    } else if msg.contains("write_operation_not_allowed") {
        (
            "operation_not_allowed",
            "Tool call rejected: operation is not allowed.",
        )
    } else if msg.contains("invalid_arguments") || msg.contains("must be a JSON object") {
        (
            "invalid_arguments",
            "Tool call rejected: malformed arguments.",
        )
    } else if msg.contains("no arguments") {
        (
            "invalid_arguments",
            "Tool call rejected: this operation takes no arguments.",
        )
    } else if msg.contains("must be between") {
        (
            "invalid_arguments",
            "Tool call rejected: argument out of range.",
        )
    } else if msg.contains("unexpected argument") {
        (
            "invalid_arguments",
            "Tool call rejected: unexpected argument.",
        )
    } else if msg.contains("must be a positive integer") {
        (
            "invalid_arguments",
            "Tool call rejected: argument must be a positive integer.",
        )
    } else if msg.contains("must be a string") {
        (
            "invalid_arguments",
            "Tool call rejected: argument must be a string.",
        )
    } else if msg.contains("not permitted") || msg.contains("capability_not_enabled") {
        (
            "operation_not_allowed",
            "Tool call rejected: operation is not allowed.",
        )
    } else {
        (
            "internal_tool_error",
            "Tool call rejected: internal tool error.",
        )
    }
}

#[cfg(test)]
#[path = "tool_loop_tests.rs"]
mod tool_loop_tests;
