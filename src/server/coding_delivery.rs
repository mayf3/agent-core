//! Production ingress wiring for the fixed Coding Intent Router.

use crate::contract_catalog::CONTRACT_CATALOG_VERSION;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::LlmClient;
use crate::runtime::{Runtime, RuntimeOutcome};
use anyhow::Result;
use serde_json::json;

pub fn matches(event: &ValidatedEvent) -> bool {
    if !matches!(event.source, EventSource::Feishu) || event.chat_type.as_deref() != Some("p2p") {
        return false;
    }
    let RuntimeEventPayload::UserMessage { text, .. } = &event.payload;
    super::coding_router::parse_coding_intent(text).is_ok()
}

pub fn deliver<L: LlmClient + 'static>(
    runtime: &Runtime<L>,
    journal: &JournalStore,
    gateway: &Gateway,
    event: ValidatedEvent,
) -> Result<RuntimeOutcome> {
    let RuntimeEventPayload::UserMessage {
        text,
        message_id,
        chat_id,
    } = event.payload.clone();
    let coding_intent = super::coding_router::parse_coding_intent(&text)?;
    let source_message_id = message_id
        .clone()
        .unwrap_or_else(|| event.dedupe_key.clone());
    let session = journal.get_or_create_session(&event.session_target)?;
    journal.append_event(
        JournalEventKind::SessionReady,
        None,
        Some(&session.id),
        Some(&event.event_id.0),
        json!({
            "session_id": session.id.0,
            "agent_id": session.agent_id.0,
            "channel": format!("{:?}", session.channel),
            "conversation_key": session.conversation_key,
        }),
    )?;
    let snapshot_id = journal.current_registry_snapshot_id()?;
    let snapshot = journal.load_registry_snapshot(&snapshot_id)?;
    let run = runtime.create_run(journal, &session, &event, &snapshot_id, &snapshot);
    let development_request = DevelopmentRequest::from_draft(
        coding_intent.development_request,
        run.principal.principal_id.0.clone(),
        session.id.0.clone(),
        source_message_id.clone(),
        format!("development:{source_message_id}"),
        CONTRACT_CATALOG_VERSION.to_string(),
    )?;
    journal.insert_run(&run)?;
    journal.append_event(
        JournalEventKind::RunStarted,
        Some(&run.id),
        Some(&session.id),
        Some(&event.event_id.0),
        json!({
            "run_id": run.id.0,
            "trigger_event_id": run.trigger_event_id.0,
            "principal_id": run.principal.principal_id.0,
            "route": "generic-development-v1",
            "development_request_id": development_request.request_id,
            "target_kind": development_request.target_kind,
            "component_profile": development_request.build_profile,
            "contract_catalog_version": development_request.contract_catalog_version,
        }),
    )?;

    let (reply, failed, proposal_id) = match super::coding_task_submit::handle_coding_task_submit(
        journal,
        gateway,
        runtime.config(),
        &development_request,
        &run,
        &session,
        &source_message_id,
    ) {
        Ok(result) => (
            format!(
                "通用开发与五项验收已完成，等待批准。\nDevelopmentRequest：{}\nProfile：{}\nProposal：{}\nArtifact：{}",
                result.development_request_id,
                result.component_profile,
                result.proposal_id,
                short_digest(&result.artifact_digest),
            ),
            false,
            Some(result.proposal_id),
        ),
        Err(error) => {
            journal.fail_run(&run.id)?;
            journal.append_event(
                JournalEventKind::RunFailed,
                Some(&run.id),
                Some(&session.id),
                Some(&source_message_id),
                json!({
                    "run_id": run.id.0,
                    "route": "generic-development-v1",
                    "development_request_id": development_request.request_id,
                    "error_category": safe_category(&error),
                }),
            )?;
            (format!("开发未完成：{}\n原因：{}", safe_category(&error), user_facing_error(&error)), true, None)
        }
    };

    let mut reply_intent = runtime.reply_intent(&run, &session, &reply, message_id, chat_id);
    if let Some(proposal_id) = proposal_id {
        if let Some(arguments) = reply_intent.arguments.as_object_mut() {
            // The Connector enforces exactly one presentation mode.  The card
            // contains the complete user-facing pending message, so do not
            // also send an ambiguous text payload.
            arguments.remove("text");
            arguments.insert(
                "presentation".to_string(),
                json!({
                    "kind": "capability_proposal_pending_v1",
                    "proposal_id": proposal_id,
                }),
            );
        }
    }
    let correlation_id = reply_intent.invocation_id.0.clone();
    journal.append_event(
        JournalEventKind::InvocationProposed,
        Some(&run.id),
        Some(&session.id),
        Some(&correlation_id),
        json!({
            "operation": reply_intent.operation,
            "idempotency_key": reply_intent.idempotency_key,
        }),
    )?;
    let approved = gateway.approve_invocation(reply_intent, &run, &session, &snapshot)?;
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
    runtime.enqueue_or_pause(
        journal,
        &approved,
        &run,
        &session,
        &correlation_id,
        &snapshot,
    )?;
    if failed {
        journal.fail_run(&run.id)?;
    }
    Ok(RuntimeOutcome {
        run_id: run.id,
        session_id: session.id,
        output: reply,
    })
}

fn short_digest(digest: &str) -> &str {
    digest.get(..19).unwrap_or(digest)
}

fn safe_category(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("capability_not_enabled") {
        "coding_owner_not_authorized"
    } else if message.contains("operation_not_allowed") {
        "coding_submit_not_registered"
    } else if message.contains("CODING_ACCEPTANCE_INFRASTRUCTURE_FAILURE") {
        "coding_infrastructure_failure"
    } else if message.contains("CANDIDATE_NOT_ACCEPTED") {
        "candidate_rejected"
    } else if message.contains("CONNECT") {
        "coding_harness_unavailable"
    } else if message.contains("SANDBOX") {
        "linux_sandbox_unavailable"
    } else if message.contains("SUBMIT_FAILED:GENERATOR_MODEL_NOT_CONFIGURED") {
        "generator_model_not_configured"
    } else if message.contains("SUBMIT_FAILED:GENERATOR_NOT_CONFIGURED_FOR_PROFILE") {
        "generator_not_configured"
    } else if message.contains("SUBMIT_FAILED:UNKNOWN_COMPONENT_PROFILE") {
        "unknown_component_profile"
    } else if message.contains("SUBMIT_FAILED:INVALID_DEVELOPMENT_REQUEST") {
        "invalid_development_request"
    } else if message.contains("SUBMIT_FAILED:CANDIDATE_GENERATION_FAILED") {
        "candidate_generation_failed"
    } else if message.contains("GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED") {
        "acceptance_repair_exhausted"
    } else if message.contains("GENERATOR_COMPILE_REPAIR_EXHAUSTED") {
        "generator_repair_exhausted"
    } else if message.contains("GENERATOR_MODEL_OUTPUT_UNSAFE") {
        "model_output_unsafe"
    } else if message.contains("GENERATOR_MODEL_OUTPUT_TRUNCATED") {
        "model_output_truncated"
    } else if message.contains("GENERATOR_MODEL_UNAVAILABLE") {
        "generator_model_unavailable"
    } else if message.contains("GENERATOR_MODEL_OUTPUT_INVALID") {
        "model_output_invalid"
    } else if message.contains("GENERATOR_COMPILE_PROBE_INFRASTRUCTURE_FAILURE") {
        "generator_infrastructure_failure"
    } else {
        "coding_flow_failed"
    }
}

fn user_facing_error(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("SUBMIT_FAILED:GENERATOR_MODEL_NOT_CONFIGURED") {
        "开发请求已进入 Coding Harness，但模型生成服务尚未配置。"
    } else if message.contains("SUBMIT_FAILED:GENERATOR_NOT_CONFIGURED_FOR_PROFILE") {
        "Coding Harness 不支持该组件类型的生成（Profile 未配置）。"
    } else if message.contains("SUBMIT_FAILED:UNKNOWN_COMPONENT_PROFILE") {
        "未知的组件 Profile。"
    } else if message.contains("SUBMIT_FAILED:INVALID_DEVELOPMENT_REQUEST") {
        "开发请求格式无效。"
    } else if message.contains("SUBMIT_FAILED:CANDIDATE_GENERATION_FAILED") {
        "候选组件生成失败。"
    } else if message.contains("GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED") {
        "候选程序未通过业务验收，已安全停止，未创建部署提案。"
    } else if message.contains("GENERATOR_COMPILE_REPAIR_EXHAUSTED") {
        "代码生成已完成，但候选程序在编译修复次数耗尽后仍未通过。"
    } else if message.contains("GENERATOR_MODEL_OUTPUT_UNSAFE") {
        "候选程序违反安全限制，已安全拒绝，未创建部署提案。"
    } else if message.contains("GENERATOR_MODEL_OUTPUT_TRUNCATED") {
        "模型输出被截断，生成不完整，请重试。"
    } else if message.contains("GENERATOR_MODEL_UNAVAILABLE")
        || message.contains("GENERATOR_COMPILE_PROBE_INFRASTRUCTURE_FAILURE")
    {
        "模型生成服务暂时不可用，请稍后重试。"
    } else if message.contains("CODING_HARNESS_CONNECT_FAILED") {
        "无法连接到 Coding Harness。"
    } else {
        "请稍后重试。"
    }
}

#[cfg(test)]
mod tests {
    use super::safe_category;
    use anyhow::anyhow;

    #[test]
    fn acceptance_infrastructure_failure_does_not_blame_candidate() {
        assert_eq!(
            safe_category(&anyhow!("CODING_ACCEPTANCE_INFRASTRUCTURE_FAILURE")),
            "coding_infrastructure_failure"
        );
        assert_eq!(
            safe_category(&anyhow!("CANDIDATE_NOT_ACCEPTED")),
            "candidate_rejected"
        );
    }

    #[test]
    fn harness_error_codes_map_to_stable_categories() {
        assert_eq!(
            safe_category(&anyhow!("CODING_HARNESS_SUBMIT_FAILED:GENERATOR_MODEL_NOT_CONFIGURED")),
            "generator_model_not_configured"
        );
        assert_eq!(
            safe_category(&anyhow!("CODING_HARNESS_SUBMIT_FAILED:GENERATOR_NOT_CONFIGURED_FOR_PROFILE")),
            "generator_not_configured"
        );
    }

    #[test]
    fn unknown_error_falls_back_to_coding_flow_failed() {
        assert_eq!(
            safe_category(&anyhow!("UNKNOWN_ERROR_SOMETHING_ELSE")),
            "coding_flow_failed"
        );
    }
}
