//! Production ingress wiring for the fixed Coding Intent Router.

use crate::contract_catalog::CONTRACT_CATALOG_VERSION;
use crate::domain::external_execution_failure::ExternalExecutionFailureClass;
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
    // Use the shared failure classification. This is the single source
    // of truth for mapping error messages to stable categories.
    ExternalExecutionFailureClass::from_message(&error.to_string()).as_str()
}

fn user_facing_error(error: &anyhow::Error) -> &'static str {
    // Use the shared failure classification for user-facing messages.
    // This ensures consistency between error categories and user messages.
    ExternalExecutionFailureClass::from_message(&error.to_string()).user_facing()
}

#[cfg(test)]
mod tests {
    use super::safe_category;
    use anyhow::anyhow;

    #[test]
    fn acceptance_infrastructure_failure_does_not_blame_candidate() {
        assert_eq!(
            safe_category(&anyhow!("CODING_ACCEPTANCE_INFRASTRUCTURE_FAILURE")),
            "external_infrastructure_failure"
        );
        assert_eq!(
            safe_category(&anyhow!("CANDIDATE_NOT_ACCEPTED")),
            "external_unavailable"
        );
    }

    #[test]
    fn harness_error_codes_map_to_stable_categories() {
        assert_eq!(
            safe_category(&anyhow!(
                "CODING_HARNESS_SUBMIT_FAILED:GENERATOR_MODEL_NOT_CONFIGURED"
            )),
            "external_configuration_missing"
        );
        assert_eq!(
            safe_category(&anyhow!(
                "CODING_HARNESS_SUBMIT_FAILED:GENERATOR_NOT_CONFIGURED_FOR_PROFILE"
            )),
            "external_configuration_missing"
        );
    }

    #[test]
    fn unknown_error_falls_back_to_external_infrastructure_failure() {
        assert_eq!(
            safe_category(&anyhow!("UNKNOWN_ERROR_SOMETHING_ELSE")),
            "external_infrastructure_failure"
        );
    }
}
