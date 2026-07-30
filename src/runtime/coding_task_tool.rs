use super::tool_loop::ToolCallOutcome;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::LlmClient;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelDevelopmentRequest {
    target_kind: TargetKind,
    name: String,
    requirements: Vec<String>,
    required_contracts: Vec<String>,
    acceptance_criteria: Vec<String>,
}

impl<L: LlmClient + 'static> super::Runtime<L> {
    pub(crate) fn dispatch_coding_task_submit(
        &self,
        approved: &ApprovedInvocation,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        correlation_id: &str,
    ) -> ToolCallOutcome {
        let result = approved
            .intent()
            .arguments
            .get("development_request")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("DEVELOPMENT_REQUEST_MISSING"))
            .and_then(|value| {
                serde_json::from_value::<ModelDevelopmentRequest>(value)
                    .map_err(|_| anyhow::anyhow!("INVALID_DEVELOPMENT_REQUEST"))
            })
            .and_then(|draft| seal_development_request(journal, run, session, draft))
            .and_then(|request| {
                crate::server::coding_task_submit::handle_coding_task_submit(
                    journal,
                    gateway,
                    &self.config,
                    &request,
                    run,
                    session,
                    &request.source_message_id,
                )
            })
            .and_then(|result| serde_json::to_value(result).map_err(Into::into));
        let (status, output, text) = match result {
            Ok(output) => (
                ReceiptStatus::Succeeded,
                output.clone(),
                serde_json::to_string(&json!({"status":"succeeded","result":output}))
                    .unwrap_or_else(|_| r#"{"status":"succeeded"}"#.into()),
            ),
            Err(error) => {
                let detail = safe_detail_code(&error.to_string());
                let output =
                    json!({"error_category":"external_execution_failed","detail_code":detail});
                (
                    ReceiptStatus::Failed,
                    output,
                    serde_json::to_string(&json!({
                        "status":"execution_failed",
                        "error_category":"external_execution_failed",
                        "detail_code":detail,
                    }))
                    .unwrap_or_else(|_| r#"{"status":"execution_failed"}"#.into()),
                )
            }
        };
        if journal
            .append_event(
                JournalEventKind::ReceiptReceived,
                Some(&run.id),
                Some(&session.id),
                Some(correlation_id),
                json!({
                    "invocation_id": approved.intent().invocation_id,
                    "operation": approved.intent().operation,
                    "status": format!("{:?}", status),
                    "output": output,
                }),
            )
            .is_err()
        {
            return ToolCallOutcome::Fatal {
                category: "journal_unwritable",
            };
        }
        ToolCallOutcome::ToolResult { text }
    }
}

fn seal_development_request(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    request: ModelDevelopmentRequest,
) -> anyhow::Result<DevelopmentRequest> {
    let ingress = journal
        .ingress_event_by_event_id(&run.trigger_event_id.0)?
        .ok_or_else(|| anyhow::anyhow!("DEVELOPMENT_REQUEST_SOURCE_EVENT_MISSING"))?;
    let source_message_id = ingress
        .payload
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("DEVELOPMENT_REQUEST_SOURCE_MESSAGE_MISSING"))?;
    let catalog = crate::contract_catalog::ContractCatalog::v1();
    let mut requested_permissions = Vec::new();
    for contract_id in &request.required_contracts {
        let contract = catalog
            .get(contract_id)
            .ok_or_else(|| anyhow::anyhow!("DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN"))?;
        for permission in &contract.permissions {
            if !requested_permissions.contains(permission) {
                requested_permissions.push(permission.clone());
            }
        }
    }
    let mut draft = DevelopmentRequestDraft::new(request.target_kind, request.name);
    draft.requirements = request.requirements;
    draft.required_contracts = request.required_contracts;
    draft.requested_permissions = requested_permissions;
    draft.acceptance_criteria = request.acceptance_criteria;
    DevelopmentRequest::from_draft(
        draft,
        run.principal.principal_id.0.clone(),
        session.id.0.clone(),
        source_message_id.into(),
        format!("development:{source_message_id}"),
        crate::contract_catalog::CONTRACT_CATALOG_VERSION.into(),
    )
}

fn safe_detail_code(message: &str) -> String {
    message
        .split(|character: char| {
            !(character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_')
        })
        .filter(|value| value.len() >= 3 && value.bytes().any(|byte| byte.is_ascii_uppercase()))
        .next_back()
        .unwrap_or("CODING_TASK_SUBMIT_FAILED")
        .chars()
        .take(128)
        .collect()
}
