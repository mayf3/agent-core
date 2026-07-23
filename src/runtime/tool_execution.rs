use super::tool_loop::ToolCallOutcome;
use super::tool_rejection::{
    internal_tool_call_id, sanitize_operation_for_audit_with_snapshot, validate_model_arguments,
};
use crate::domain::*;
use crate::gateway::{Gateway, ToolRejection};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, ToolCall};
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::Result;
use serde_json::json;
use std::time::Duration;
pub(super) fn append_or_fatal(
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
fn rejected_result(
    rejection: ToolRejection,
    parameters: Option<&serde_json::Value>,
) -> ToolCallOutcome {
    let text = match &rejection {
        ToolRejection::InvalidArgumentsWithDetails(issue) => {
            use crate::registry::schema::SchemaValidationIssue;
            let mut details = serde_json::json!({"retryable": true});
            match issue.as_ref() {
                SchemaValidationIssue::MissingRequired { fields } => {
                    details["error_category"] = serde_json::json!("invalid_arguments");
                    details["missing_fields"] = serde_json::json!(fields);
                    // If workspace_id is missing, extract available IDs from pinned schema.
                    if fields.contains(&"workspace_id".to_string()) {
                        if let Some(params) = parameters {
                            if let Some(ws_enum) = params
                                .pointer("/properties/workspace_id/enum")
                                .and_then(|v| v.as_array())
                            {
                                let ids: Vec<String> = ws_enum
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .map(String::from)
                                    .collect();
                                if !ids.is_empty() {
                                    details["available_workspace_ids"] = serde_json::json!(ids);
                                }
                            }
                        }
                    }
                }
                SchemaValidationIssue::EnumMismatch { property, allowed } => {
                    details["error_category"] = serde_json::json!("invalid_arguments");
                    if let Some(p) = property {
                        details["invalid_field"] = serde_json::json!(p);
                    }
                    details["allowed_values"] = serde_json::json!(allowed);
                }
                SchemaValidationIssue::UnexpectedProperty { property } => {
                    details["error_category"] = serde_json::json!("invalid_arguments");
                    details["unexpected_field"] = serde_json::json!(property);
                }
                _ => {
                    details["error_category"] = serde_json::json!("invalid_arguments");
                }
            }
            serde_json::to_string(&serde_json::json!({
                "status": "rejected",
                "error_category": "invalid_arguments",
                "retryable": true,
                "details": details,
            }))
            .unwrap_or_else(|_| {
                format!("status: rejected\nerror_category: {}", rejection.category())
            })
        }
        _ => format!(
            "status: rejected\nerror_category: {}\nmessage: {}",
            rejection.category(),
            rejection.safe_message()
        ),
    };
    ToolCallOutcome::ToolResult { text }
}
pub(crate) use super::tool_dispatch::dispatch_builtin_binding;
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
        Ok(rejected_result(ToolRejection::MalformedToolCall, None))
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
        let audited_op = sanitize_operation_for_audit_with_snapshot(&tool_call.operation, snapshot);
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
                    journal,
                    run,
                    session,
                    &tool_call.id,
                    &audited_op,
                    crate::gateway::ToolRejection::UnknownOperation,
                );
            }
        };
        let mut intent = match crate::gateway::validate_tool_call(
            tool_call, &run.id, turn_index, tool_index, snapshot,
        ) {
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
        if let Err(rejection) = validate_model_arguments(spec, &intent.arguments) {
            // Use rejected_result directly with spec parameters so
            // available_workspace_ids can be extracted from the pinned schema.
            if let Some(fatal) = append_or_fatal(
                journal,
                JournalEventKind::ToolCallRejected,
                run,
                session,
                None,
                json!({"operation": audited_op, "tool_call_id": tool_call.id, "error_category": rejection.category()}),
            ) {
                return Ok(fatal);
            }
            return Ok(rejected_result(rejection, Some(&spec.parameters)));
        }
        // Inject session_id for policy session-scope check. External harness
        // dispatch strips it before sending to the harness.
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
        let approved = match gateway.approve_invocation(intent, run, session, snapshot) {
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
                return Ok(rejected_result(ToolRejection::PolicyDenied, None));
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

        // HCR mode: per-dispatch revalidation (principal, channel,
        // conversation, owner, HCR state, claim, harness, workspace) +
        // operation allowlist + workspace validation.
        if matches!(run.mode, RunMode::Hcr { .. }) {
            let is_owner =
                super::coding_grants::is_coding_owner(&self.config, &run.principal, Some("p2p"));
            if let Err(_) = crate::hcr::revalidate::revalidate_hcr_dispatch_context(
                journal, run, session, is_owner,
            ) {
                return Ok(rejected_result(
                    crate::gateway::ToolRejection::PolicyDenied,
                    None,
                ));
            }
            if !crate::runtime::coding_grants::is_hcr_allowed_operation(
                &approved.intent().operation,
            ) {
                return Ok(rejected_result(
                    crate::gateway::ToolRejection::OperationNotAllowed,
                    None,
                ));
            }
            // Validate workspace_id for workspace operations.
            if approved.intent().operation != crate::domain::operation::external::WORKSPACE_LIST {
                if let Some(ws_id) = approved
                    .intent()
                    .arguments
                    .get("workspace_id")
                    .and_then(|v| v.as_str())
                {
                    if let Err(_) = crate::hcr::revalidate::validate_hcr_workspace(ws_id) {
                        return Ok(rejected_result(
                            crate::gateway::ToolRejection::InvalidArguments,
                            None,
                        ));
                    }
                }
            }
        }

        if approved.intent().operation == crate::domain::operation::external::TASK_SUBMIT {
            return Ok(self.dispatch_coding_task_submit(
                &approved,
                journal,
                gateway,
                run,
                session,
                &correlation_id,
            ));
        }

        return Ok(dispatch_builtin_binding(
            spec,
            &approved,
            journal,
            run,
            session,
            &correlation_id,
            Duration::from_millis(self.config.harness_read_timeout_ms),
            &snapshot.snapshot_id,
        ));
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
        Ok(rejected_result(rejection, None))
    }
}
