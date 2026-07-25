use super::tool_execution::append_or_fatal;
use super::tool_loop::ToolCallOutcome;
use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::Result;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) enum ToolDispatchError {
    RetiredBuiltinOperation(String),
    UnknownBuiltinBinding(String),
    HarnessManifestNotFound(String),
    HarnessManifestLoadFailed(String),
}

impl ToolDispatchError {
    pub fn error_category(&self) -> &'static str {
        match self {
            Self::RetiredBuiltinOperation(_) => "retired_builtin_operation",
            Self::UnknownBuiltinBinding(_) => "registry_binding_invalid",
            Self::HarnessManifestNotFound(_) => "external_manifest_not_found",
            Self::HarnessManifestLoadFailed(_) => "external_manifest_load_failed",
        }
    }
}

impl std::fmt::Display for ToolDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RetiredBuiltinOperation(key) => write!(f, "retired_builtin_operation: {key}"),
            Self::UnknownBuiltinBinding(key) => write!(f, "registry_binding_invalid: {key}"),
            Self::HarnessManifestNotFound(id) => write!(f, "external_manifest_not_found: {id}"),
            Self::HarnessManifestLoadFailed(message) => {
                write!(f, "external_manifest_load_failed: {message}")
            }
        }
    }
}

impl std::error::Error for ToolDispatchError {}

pub(crate) fn dispatch_builtin_binding(
    spec: &crate::registry::snapshot::OperationSpec,
    approved: &ApprovedInvocation,
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    correlation_id: &str,
    harness_read_timeout: Duration,
    registry_snapshot_id: &str,
) -> ToolCallOutcome {
    let receipt_result: Result<Receipt> = match spec.binding_key.as_str() {
        "builtin.session_recall_recent" => {
            super::tool_rejection::execute_session_recall(journal, &session.id, approved).map(
                |(status, output, _)| Receipt {
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
        "builtin.time_now" => Err(anyhow::Error::from(
            ToolDispatchError::RetiredBuiltinOperation(spec.binding_key.clone()),
        )),
        _ if spec.binding_kind == crate::registry::snapshot::BindingKind::External => {
            let manifest_id = &spec.binding_key;
            match journal.load_harness_manifest(manifest_id) {
                Ok(Some(manifest)) => {
                    let transport =
                        crate::adapters::external_harness::ExternalHarnessTransportConfig {
                            read_timeout: harness_read_timeout,
                            ..Default::default()
                        };
                    crate::adapters::external_harness::execute_external_harness_with_config(
                        &manifest,
                        approved,
                        &transport,
                        registry_snapshot_id,
                    )
                }
                Ok(None) => Err(anyhow::Error::from(
                    ToolDispatchError::HarnessManifestNotFound(manifest_id.clone()),
                )),
                Err(error) => Err(anyhow::Error::from(
                    ToolDispatchError::HarnessManifestLoadFailed(error.to_string()),
                )),
            }
        }
        _ => Err(anyhow::Error::from(
            ToolDispatchError::UnknownBuiltinBinding(spec.binding_key.clone()),
        )),
    };
    let (status, output, external_ref, text) = map_receipt(receipt_result);
    if let Some(fatal) = append_or_fatal(
        journal,
        JournalEventKind::ReceiptReceived,
        run,
        session,
        Some(correlation_id),
        json!({
            "invocation_id": approved.intent().invocation_id,
            "operation": approved.intent().operation,
            "failed_stage": (status == ReceiptStatus::Failed).then_some("external_execution"),
            "status": format!("{:?}", status),
            "output": output,
            "external_ref": external_ref,
        }),
    ) {
        return fatal;
    }
    ToolCallOutcome::ToolResult { text }
}

fn map_receipt(
    result: Result<Receipt>,
) -> (ReceiptStatus, serde_json::Value, Option<String>, String) {
    match result {
        Ok(receipt) => {
            let text = receipt_text(&receipt);
            (receipt.status, receipt.output, receipt.external_ref, text)
        }
        Err(error) => {
            let category = error_category(&error);
            (
                ReceiptStatus::Failed,
                json!({"error_category": category}),
                None,
                format!("status: execution_failed\nerror_category: {category}"),
            )
        }
    }
}

fn receipt_text(receipt: &Receipt) -> String {
    match receipt.status {
        ReceiptStatus::Succeeded => format!("status: succeeded\noutput: {:?}", receipt.output),
        ReceiptStatus::Unknown => {
            "status: execution_failed\nerror_category: unknown_outcome".into()
        }
        ReceiptStatus::Failed => {
            let category = receipt
                .output
                .get("error_category")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("harness_failed");
            let mut text = format!("status: execution_failed\nerror_category: {category}");
            if let Some(detail) = receipt
                .output
                .get("detail_code")
                .and_then(serde_json::Value::as_str)
            {
                text.push_str("\ndetail_code: ");
                text.push_str(detail);
            }
            if let Some(code) = receipt
                .output
                .get("http_code")
                .and_then(serde_json::Value::as_u64)
            {
                text.push_str(&format!("\nhttp_code: {code}"));
            }
            text
        }
    }
}

fn error_category(error: &anyhow::Error) -> &'static str {
    if let Some(error) = error.downcast_ref::<ToolDispatchError>() {
        return error.error_category();
    }
    let message = error.to_string();
    if message.contains("timed out") || message.contains("timeout") {
        "timeout"
    } else if message.contains("connect failed") {
        "connect_failed"
    } else if message.contains("protocol") {
        "protocol_mismatch"
    } else if message.contains("non-2xx") || message.contains("HTTP") {
        "http_error"
    } else if message.contains("schema violation") || message.contains("output schema") {
        "output_schema_violation"
    } else if message.contains("exceeds 64 KiB") {
        "response_too_large"
    } else if message.contains("malformed")
        || message.contains("invalid JSON")
        || message.contains("UTF-8")
    {
        "malformed_response"
    } else {
        "harness_failed"
    }
}
