//! Production ingress wiring for the fixed Coding Intent Router.

use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::LlmClient;
use crate::runtime::{Runtime, RuntimeOutcome};
use anyhow::Result;
use serde_json::json;

pub fn matches(event: &ValidatedEvent) -> bool {
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
            "route": "calculator-v0",
        }),
    )?;

    let (reply, failed) = match super::coding_task_submit::handle_coding_task_submit(
        journal,
        gateway,
        runtime.config(),
        &coding_intent,
        &run,
        &session,
        &source_message_id,
    ) {
        Ok(result) => (
            format!(
                "开发与五项验收已完成，等待批准。\nProposal：{}\nArtifact：{}",
                result.proposal_id,
                short_digest(&result.artifact_digest),
            ),
            false,
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
                    "route": "calculator-v0",
                    "error_category": safe_category(&error),
                }),
            )?;
            (format!("开发未完成：{}", safe_category(&error)), true)
        }
    };

    let reply_intent = runtime.reply_intent(&run, &session, &reply, message_id, chat_id);
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
    } else if message.contains("CANDIDATE_NOT_ACCEPTED") {
        "candidate_rejected"
    } else if message.contains("CONNECT") {
        "coding_harness_unavailable"
    } else if message.contains("SANDBOX") {
        "linux_sandbox_unavailable"
    } else {
        "coding_flow_failed"
    }
}
