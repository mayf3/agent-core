use super::tool_loop::ToolCallOutcome;
use super::tool_rejection::{
    internal_tool_call_id, sanitize_operation_for_audit, validate_model_arguments,
};
use crate::adapters::InvocationAdapter;
use crate::domain::*;
use crate::gateway::{Gateway, ToolRejection};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, ToolCall};
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::Result;
use serde_json::json;

fn append_or_fatal(
    journal: &JournalStore,
    kind: JournalEventKind,
    run: &Run,
    session: &Session,
    correlation_id: Option<&str>,
    payload: serde_json::Value,
) -> Option<ToolCallOutcome> {
    journal
        .append_event(
            kind,
            Some(&run.id),
            Some(&session.id),
            correlation_id,
            payload,
        )
        .err()
        .map(|_| ToolCallOutcome::Fatal {
            category: "journal_unwritable",
        })
}

fn rejected_result(rejection: ToolRejection) -> ToolCallOutcome {
    ToolCallOutcome::ToolResult {
        text: format!(
            "status: rejected\nerror_category: {}\nmessage: {}",
            rejection.category(),
            rejection.safe_message()
        ),
    }
}

impl<L: LlmClient + 'static> super::Runtime<L> {
    pub(crate) fn handle_malformed_tool_call(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        turn_index: usize,
        tool_index: usize,
    ) -> Result<ToolCallOutcome> {
        let internal_id = internal_tool_call_id(&run.id.0, turn_index, tool_index);
        for kind in [
            JournalEventKind::ToolCallIssued,
            JournalEventKind::ToolCallRejected,
        ] {
            let payload = if kind == JournalEventKind::ToolCallIssued {
                json!({"operation": "malformed_tool_call", "tool_call_id": internal_id})
            } else {
                json!({
                    "operation": "malformed_tool_call",
                    "tool_call_id": internal_id,
                    "error_category": ToolRejection::MalformedToolCall.category(),
                })
            };
            if let Some(fatal) = append_or_fatal(journal, kind, run, session, None, payload) {
                return Ok(fatal);
            }
        }
        Ok(rejected_result(ToolRejection::MalformedToolCall))
    }

    pub(crate) fn handle_inline_tool_call(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        tool_call: &ToolCall,
        turn_index: usize,
        tool_index: usize,
        snapshot: &RegistrySnapshot,
    ) -> Result<ToolCallOutcome> {
        let audited_op = sanitize_operation_for_audit(&tool_call.operation);

        // Always write ToolCallIssued first (audit trail), even for operations
        // that will be rejected by the snapshot pre-check below.
        if let Some(fatal) = append_or_fatal(
            journal,
            JournalEventKind::ToolCallIssued,
            run,
            session,
            None,
            json!({"operation": audited_op, "tool_call_id": tool_call.id}),
        ) {
            return Ok(fatal);
        }

        // Look up the operation in the Run's pinned RegistrySnapshot.
        // This is the single authoritative source — Gateway and dispatch both
        // use the resolved operation definition, never the static catalog.
        let spec = match snapshot.lookup(&tool_call.operation) {
            Some(s) => s,
            None => {
                return self.record_rejection(
                    journal, run, session, &tool_call.id,
                    &audited_op, crate::gateway::ToolRejection::UnknownOperation,
                );
            }
        };

        let mut intent =
            match crate::gateway::validate_tool_call(tool_call, &run.id, turn_index, tool_index) {
                Ok(intent) => intent,
                Err(rejection) => {
                    return self.record_rejection(
                        journal,
                        run,
                        session,
                        &tool_call.id,
                        &audited_op,
                        rejection,
                    )
                }
            };
        if let Err(rejection) = validate_model_arguments(&intent.operation, &intent.arguments) {
            return self.record_rejection(
                journal,
                run,
                session,
                &tool_call.id,
                &audited_op,
                rejection,
            );
        }
        if let Some(arguments) = intent.arguments.as_object_mut() {
            arguments.insert("session_id".to_string(), json!(session.id.0));
        }
        let correlation_id = intent.invocation_id.0.clone();
        if let Some(fatal) = append_or_fatal(
            journal,
            JournalEventKind::InvocationProposed,
            run,
            session,
            Some(&correlation_id),
            json!({
                "operation": intent.operation,
                "idempotency_key": intent.idempotency_key,
                "source": "model_tool_call",
            }),
        ) {
            return Ok(fatal);
        }

        let approved = match gateway.approve_invocation(intent, run, session) {
            Ok(approved) => approved,
            Err(_) => {
                if let Some(fatal) = append_or_fatal(
                    journal,
                    JournalEventKind::ToolCallRejected,
                    run,
                    session,
                    Some(&correlation_id),
                    json!({
                        "operation": "tool_call",
                        "invocation_id": correlation_id,
                        "error_category": ToolRejection::PolicyDenied.category(),
                    }),
                ) {
                    return Ok(fatal);
                }
                return Ok(rejected_result(ToolRejection::PolicyDenied));
            }
        };
        if let Some(fatal) = append_or_fatal(
            journal,
            JournalEventKind::InvocationApproved,
            run,
            session,
            Some(&correlation_id),
            json!({
                "decision_id": approved.decision_id,
                "operation": approved.intent().operation,
            }),
        ) {
            return Ok(fatal);
        }

        let exec_result: Result<(serde_json::Value, String)> =
            match spec.binding_key.as_str() {
                "builtin.time_now" => crate::adapters::TimeAdapter
                    .execute(&approved)
                    .map(|receipt| {
                        let text = receipt
                            .output
                            .get("iso")
                            .and_then(|value| value.as_str())
                            .unwrap_or("ok")
                            .to_string();
                        (receipt.output, text)
                    }),
                "builtin.session_recall_recent" => {
                    Self::execute_session_recall(journal, &session.id, &approved)
                        .map(|(_, output, text)| (output, text))
                }
                "builtin.system_status" => crate::capabilities::execute(journal)
                    .map(|output| {
                        let text = output
                            .get("summary")
                            .and_then(|value| value.as_str())
                            .unwrap_or("ok")
                            .to_string();
                        (output, text)
                    }),
                _ => Err(anyhow::anyhow!("registry_binding_invalid: {}", spec.binding_key)),
            };
        let (status, output, text) = match exec_result {
            Ok((output, text)) => (
                ReceiptStatus::Succeeded,
                output,
                format!("status: succeeded\noutput: {text}"),
            ),
            Err(_) => (
                ReceiptStatus::Failed,
                json!({"error_category": "capability_execution_failed"}),
                "status: execution_failed\nerror_category: capability_execution_failed".to_string(),
            ),
        };
        if let Some(fatal) = append_or_fatal(
            journal,
            JournalEventKind::ReceiptReceived,
            run,
            session,
            Some(&correlation_id),
            json!({
                "invocation_id": approved.intent().invocation_id,
                "status": format!("{:?}", status),
                "output": output,
            }),
        ) {
            return Ok(fatal);
        }
        Ok(ToolCallOutcome::ToolResult { text })
    }

    fn record_rejection(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        tool_call_id: &str,
        audited_op: &str,
        rejection: ToolRejection,
    ) -> Result<ToolCallOutcome> {
        if let Some(fatal) = append_or_fatal(
            journal,
            JournalEventKind::ToolCallRejected,
            run,
            session,
            None,
            json!({
                "operation": audited_op,
                "tool_call_id": tool_call_id,
                "error_category": rejection.category(),
            }),
        ) {
            return Ok(fatal);
        }
        Ok(rejected_result(rejection))
    }
}
