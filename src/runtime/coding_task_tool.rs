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
            Ok(output) => {
                let text = serde_json::to_string(&json!({
                    "status": "succeeded",
                    "result": output,
                }))
                .unwrap_or_else(|_| r#"{"status":"succeeded"}"#.into());
                (ReceiptStatus::Succeeded, output, text)
            }
            Err(error) => {
                let class = crate::domain::external_execution_failure::ExternalExecutionFailureClass::from_message(
                    &error.to_string(),
                );
                let detail_code = error
                    .downcast_ref::<crate::server::coding_task_submit::CodingHarnessRejection>()
                    .map(|value| value.code.clone())
                    .unwrap_or_else(|| safe_detail_code(&error.to_string()));
                let output = json!({
                    "error_category": class.as_str(),
                    "detail_code": detail_code,
                });
                let text = serde_json::to_string(&json!({
                    "status": "execution_failed",
                    "error_category": class.as_str(),
                    "detail_code": detail_code,
                }))
                .unwrap_or_else(|_| r#"{"status":"execution_failed"}"#.into());
                (ReceiptStatus::Failed, output, text)
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
                    "failed_stage": (status == ReceiptStatus::Failed).then_some("external_execution"),
                    "status": format!("{:?}", status),
                    "output": output,
                    "external_ref": Value::Null,
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
        source_message_id.to_string(),
        format!("development:{source_message_id}"),
        crate::contract_catalog::CONTRACT_CATALOG_VERSION.to_string(),
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

#[cfg(test)]
mod tests {
    use super::{safe_detail_code, seal_development_request, ModelDevelopmentRequest};
    use crate::domain::*;
    use crate::journal::JournalStore;
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn detail_code_is_opaque_and_bounded() {
        assert_eq!(
            safe_detail_code("coding harness rejected: GENERATOR_NOT_CONFIGURED_FOR_PROFILE"),
            "GENERATOR_NOT_CONFIGURED_FOR_PROFILE"
        );
        assert_eq!(
            safe_detail_code("arbitrary text"),
            "CODING_TASK_SUBMIT_FAILED"
        );
    }

    #[test]
    fn model_request_contains_semantics_only() {
        let request: ModelDevelopmentRequest = serde_json::from_value(json!({
            "target_kind": "invocable_capability",
            "name": "external.failure_viewer_query",
            "requirements": ["query failure-viewer /api/state"],
            "required_contracts": ["component.invoke.v0"],
            "acceptance_criteria": ["return latest failed Receipt facts"]
        }))
        .unwrap();
        assert_eq!(request.target_kind, TargetKind::InvocableCapability);
        assert!(serde_json::from_value::<ModelDevelopmentRequest>(json!({
            "target_kind": "invocable_capability",
            "name": "external.failure_viewer_query",
            "requirements": ["query failure-viewer /api/state"],
            "required_contracts": ["component.invoke.v0"],
            "acceptance_criteria": ["return latest failed Receipt facts"],
            "build_profile": "invocable-capability-v0"
        }))
        .is_err());
    }

    #[test]
    fn kernel_seals_authenticated_origin_profiles_and_permissions() {
        let journal = JournalStore::in_memory().unwrap();
        let session = journal
            .get_or_create_session(&SessionTarget {
                agent_id: AgentId("main".into()),
                channel: ChannelKind::Feishu,
                conversation_key: "feishu:open_id:owner".into(),
            })
            .unwrap();
        let event_id = EventId("event-seal".into());
        let event = ValidatedEvent {
            event_id: event_id.clone(),
            source: EventSource::Feishu,
            principal: principal(),
            session_target: SessionTarget {
                agent_id: AgentId("main".into()),
                channel: ChannelKind::Feishu,
                conversation_key: "feishu:open_id:owner".into(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text: "create the query capability".into(),
                message_id: Some("om-authenticated".into()),
                chat_id: Some("oc-owner".into()),
            },
            dedupe_key: "feishu:om-authenticated".into(),
            occurred_at: Utc::now(),
            chat_type: Some("p2p".into()),
        };
        journal
            .accept_ingress_with_worker_job(
                &event,
                json!({"event_id": event_id.0, "message_id": "om-authenticated"}),
            )
            .unwrap();
        let run = Run {
            id: RunId("run-seal".into()),
            session_id: session.id.clone(),
            agent_id: session.agent_id.clone(),
            trigger_event_id: event_id,
            principal: principal(),
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            registry_snapshot_id: journal.current_registry_snapshot_id().unwrap(),
            mode: RunMode::Default,
        };
        let request = seal_development_request(
            &journal,
            &run,
            &session,
            ModelDevelopmentRequest {
                target_kind: TargetKind::InvocableCapability,
                name: "external.failure_viewer_query".into(),
                requirements: vec!["query failure-viewer /api/state".into()],
                required_contracts: vec!["component.invoke.v0".into()],
                acceptance_criteria: vec!["return latest failed Receipt facts".into()],
            },
        )
        .unwrap();
        assert_eq!(request.source_subject, "feishu:open_id:owner");
        assert_eq!(request.source_scope, session.id.0);
        assert_eq!(request.source_message_id, "om-authenticated");
        assert_eq!(request.build_profile, "invocable-capability-v0");
        assert_eq!(request.deployment_profile, "capability-host-v0");
        assert_eq!(request.requested_permissions, ["component.invoke"]);
        assert_eq!(
            request.contract_catalog_version,
            crate::contract_catalog::CONTRACT_CATALOG_VERSION
        );
    }

    fn principal() -> RunPrincipal {
        RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:owner".into()),
            subject: PrincipalSubject::FeishuOpenId("owner".into()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: Some("feishu:open_id:owner".into()),
        }
    }
}
