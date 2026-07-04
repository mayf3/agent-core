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
    let text = match &rejection {
        ToolRejection::InvalidArgumentsWithDetails(issue) => {
            use crate::registry::schema::SchemaValidationIssue;
            let mut details = serde_json::json!({"retryable": true});
            match issue.as_ref() {
                SchemaValidationIssue::MissingRequired { fields } => {
                    details["error_category"] = serde_json::json!("invalid_arguments");
                    details["missing_fields"] = serde_json::json!(fields);
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
/// Typed dispatch error — maps to fixed error_category, no string matching.
#[derive(Debug, Clone)]
pub(crate) enum ToolDispatchError {
    RetiredBuiltinOperation(String),
    UnknownBuiltinBinding(String),
}
impl ToolDispatchError {
    pub fn error_category(&self) -> &'static str {
        match self {
            Self::RetiredBuiltinOperation(_) => "retired_builtin_operation",
            Self::UnknownBuiltinBinding(_) => "registry_binding_invalid",
        }
    }
}
impl std::fmt::Display for ToolDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RetiredBuiltinOperation(key) => write!(f, "retired_builtin_operation: {key}"),
            Self::UnknownBuiltinBinding(key) => write!(f, "registry_binding_invalid: {key}"),
        }
    }
}
impl std::error::Error for ToolDispatchError {}
/// Authoritative binding_key → handler dispatch. External dispatch preserves
/// the adapter's actual receipt status.
pub(crate) fn dispatch_builtin_binding(
    spec: &crate::registry::snapshot::OperationSpec,
    approved: &ApprovedInvocation,
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    correlation_id: &str,
    harness_read_timeout: Duration,
) -> ToolCallOutcome {
    let receipt_result: Result<Receipt> = match spec.binding_key.as_str() {
        "builtin.session_recall_recent" => {
            super::tool_rejection::execute_session_recall(journal, &session.id, approved).map(
                |(status, output, _text)| Receipt {
                    invocation_id: approved.intent().invocation_id.clone(),
                    status,
                    output,
                    external_ref: None,
                    occurred_at: chrono::Utc::now(),
                },
            )
        }
        "builtin.system_status" => crate::capabilities::execute(journal).map(|output| Receipt {
            invocation_id: approved.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            output,
            external_ref: None,
            occurred_at: chrono::Utc::now(),
        }),
        _ if spec.binding_key == "builtin.time_now" => {
            // Retired builtin operation — no longer has a runtime handler.
            // Historical Runs referencing this binding get fail-closed.
            Err(anyhow::Error::from(
                ToolDispatchError::RetiredBuiltinOperation(spec.binding_key.clone()),
            ))
        }
        _ => {
            if spec.binding_kind == crate::registry::snapshot::BindingKind::External {
                let manifest_id = &spec.binding_key;
                match journal.load_harness_manifest(manifest_id) {
                    Ok(Some(manifest)) => {
                        let transport_config =
                            crate::adapters::external_harness::ExternalHarnessTransportConfig {
                                read_timeout: harness_read_timeout,
                                ..Default::default()
                            };
                        crate::adapters::external_harness::execute_external_harness_with_config(
                            &manifest,
                            approved,
                            &transport_config,
                        )
                    }
                    Ok(None) => Err(anyhow::anyhow!(
                        "external_harness_manifest_not_found: {manifest_id}"
                    )),
                    Err(e) => Err(anyhow::anyhow!(
                        "external_harness_manifest_load_failed: {e}"
                    )),
                }
            } else {
                Err(anyhow::Error::from(
                    ToolDispatchError::UnknownBuiltinBinding(spec.binding_key.clone()),
                ))
            }
        }
    };
    let (status, output, text) = match receipt_result {
        Ok(receipt) => {
            let text = match receipt.status {
                ReceiptStatus::Succeeded => {
                    format!("status: succeeded\noutput: {:?}", receipt.output)
                }
                ReceiptStatus::Failed => {
                    let cat: &str = receipt
                        .output
                        .get("error_category")
                        .and_then(|v: &serde_json::Value| v.as_str())
                        .unwrap_or("harness_failed");
                    format!("status: execution_failed\nerror_category: {cat}")
                }
                ReceiptStatus::Unknown => {
                    "status: execution_failed\nerror_category: unknown_outcome".to_string()
                }
            };
            (receipt.status, receipt.output, text)
        }
        Err(e) => {
            // First try typed ToolDispatchError downcast for precise category.
            // Fall back to string-based categorization for external harness errors.
            let cat = if let Some(de) = e.downcast_ref::<ToolDispatchError>() {
                de.error_category()
            } else if e.to_string().contains("timed out") || e.to_string().contains("timeout") {
                "timeout"
            } else if e.to_string().contains("connect failed") {
                "connect_failed"
            } else if e.to_string().contains("protocol version mismatch")
                || e.to_string().contains("protocol")
            {
                "protocol_mismatch"
            } else if e.to_string().contains("non-2xx") || e.to_string().contains("HTTP") {
                "http_error"
            } else if e.to_string().contains("schema violation")
                || e.to_string().contains("output schema")
            {
                "output_schema_violation"
            } else if e.to_string().contains("exceeds 64 KiB") {
                "response_too_large"
            } else if e.to_string().contains("malformed")
                || e.to_string().contains("invalid JSON")
                || e.to_string().contains("UTF-8")
            {
                "malformed_response"
            } else {
                "harness_failed"
            };
            (
                ReceiptStatus::Failed,
                json!({"error_category": cat}),
                format!("status: execution_failed\nerror_category: {cat}"),
            )
        }
    };
    if let Some(fatal) = append_or_fatal(
        journal,
        JournalEventKind::ReceiptReceived,
        run,
        session,
        Some(correlation_id),
        json!({
            "invocation_id": approved.intent().invocation_id,
            "status": format!("{:?}", status),
            "output": output,
        }),
    ) {
        return fatal;
    }
    ToolCallOutcome::ToolResult { text }
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
            return self.record_rejection(
                journal,
                run,
                session,
                &tool_call.id,
                &audited_op,
                rejection,
            );
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
        return Ok(dispatch_builtin_binding(
            spec,
            &approved,
            journal,
            run,
            session,
            &correlation_id,
            Duration::from_millis(self.config.harness_read_timeout_ms),
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
        Ok(rejected_result(rejection))
    }
}
