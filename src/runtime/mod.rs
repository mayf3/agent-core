use crate::config::KernelConfig;
use crate::context::ContextAssembler;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput};
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::json;

pub mod outbox_dispatcher;
mod tool_execution;
mod tool_loop;
mod tool_rejection;

pub use crate::gateway::ToolRejection;
pub use tool_rejection::validate_model_arguments;

#[cfg(test)]
#[path = "tests/registry_snapshot_provider_context.rs"]
mod registry_snapshot_provider_context;

#[cfg(test)]
#[path = "tests/registry_snapshot_recovery_failure.rs"]
mod registry_snapshot_recovery_failure;

#[cfg(test)]
#[path = "tests/registry_snapshot_failure.rs"]
mod registry_snapshot_failure;

#[cfg(test)]
#[path = "tests/registry_snapshot_gateway.rs"]
mod registry_snapshot_gateway;

#[cfg(test)]
#[path = "tests/external_harness_hotload.rs"]
mod external_harness_hotload;

#[cfg(test)]
#[path = "tests/external_harness_transport.rs"]
mod external_harness_transport;

#[cfg(test)]
#[path = "tests/external_harness_runtime.rs"]
mod external_harness_runtime;

#[cfg(test)]
#[path = "tests/external_harness_pinning.rs"]
mod external_harness_pinning;

pub struct Runtime<L> {
    config: KernelConfig,
    llm: L,
}

pub struct RuntimeOutcome {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub output: String,
}

pub fn session_spawn() -> Result<()> {
    bail!("not_enabled:session.spawn")
}

pub fn run_yield() -> Result<()> {
    bail!("not_enabled:run.yield")
}

impl<L> Runtime<L>
where
    L: LlmClient + 'static,
{
    pub fn new(config: KernelConfig, llm: L) -> Self {
        Self { config, llm }
    }

    /// Phase 2 M2d: decide whether an approved invocation is dispatched now or
    /// paused for human approval. ReadOnly ops queue immediately; Write ops
    /// pause when require_write_approval is enabled. Risk is determined from
    /// the Run's pinned registry snapshot, not the static catalog.
    fn enqueue_or_pause(
        &self,
        journal: &JournalStore,
        approved: &ApprovedInvocation,
        run: &Run,
        session: &Session,
        correlation_id: &str,
        snapshot: &RegistrySnapshot,
    ) -> Result<()> {
        let is_write = snapshot
            .lookup(&approved.intent().operation)
            .map(|spec| spec.risk == crate::registry::snapshot::Risk::Write)
            .unwrap_or(true);
        let pause = self.config.require_write_approval && is_write;
        if pause {
            journal.append_event(
                JournalEventKind::ApprovalRequested,
                Some(&run.id),
                Some(&session.id),
                Some(correlation_id),
                json!({
                    "operation": approved.intent().operation,
                    "decision_id": approved.decision_id,
                    "invocation_id": approved.intent().invocation_id.0,
                    "run_id": run.id.0,
                    "session_id": session.id.0,
                    "arguments": approved.intent().arguments,
                    "idempotency_key": approved.intent().idempotency_key,
                }),
            )?;
            journal.update_run_status(&run.id, "AwaitingApproval")?;
            return Ok(());
        }
        journal.queue_outbox_dispatch(approved, Some(&session.id))?;
        journal.update_run_status(&run.id, "WaitingDispatch")?;
        Ok(())
    }

    pub fn deliver(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        event: ValidatedEvent,
    ) -> Result<RuntimeOutcome> {
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
        // Blocker 2: snapshot_id must exist and be non-empty; failure prevents Run creation.
        let snapshot_id = journal
            .current_registry_snapshot_id()
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        if snapshot_id.is_empty() {
            anyhow::bail!("registry_snapshot_invalid: snapshot ID is empty");
        }
        // Load the snapshot BEFORE creating the Run. If the snapshot is
        // missing or corrupt, the error is deterministic
        // (registry_snapshot_unavailable) and no Run artifacts are created.
        let snapshot = journal
            .load_registry_snapshot(&snapshot_id)
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        let run = self.create_run(&session, &event, &snapshot_id, &snapshot);
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
            }),
        )?;

        let RuntimeEventPayload::UserMessage {
            text,
            message_id,
            chat_id,
        } = event.payload.clone();

        let granted_operations: Vec<String> = run
            .principal
            .grants
            .iter()
            .map(|g| g.operation.clone())
            .collect();

        // The loaded snapshot (Arc clone) is used throughout the Run's
        // lifetime for Context, Provider tools, and Gateway validation.

        let mut blocks = ContextAssembler::from_config(&self.config).build(
            journal,
            &session,
            &event,
            &text,
            &granted_operations,
            &snapshot,
        )?;
        journal.append_event(
            JournalEventKind::ContextBuilt,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({
                "block_count": blocks.len(),
                "kinds": blocks.iter().map(|block| format!("{:?}", block.kind)).collect::<Vec<_>>(),
            }),
        )?;
        // Provider tools are derived from the Run's pinned registry snapshot
        // once here. All LLM rounds for this Run reuse the same tools list.
        let provider_tools = snapshot.provider_tools_for_grants(&granted_operations);

        let first = self.llm.complete(LlmInput {
            blocks: blocks.clone(),
            user_text: text.clone(),
            granted_operations: granted_operations.clone(),
            provider_tools: provider_tools.clone(),
            follow_ups: vec![],
        })?;
        journal.append_event(
            JournalEventKind::LlmCompleted,
            Some(&run.id),
            Some(&session.id),
            None,
            first.journal_payload.clone(),
        )?;

        // Session Recall Loop (Task 1): when the first LLM round emits a
        // read-only tool call, execute it, append a ToolResult block, and
        // re-invoke the LLM. Bounded by MAX_TOOL_ROUNDS; a no-op when the model
        // emits no tool call (backwards compatible).
        let llm = self.run_tool_recall_loop(
            journal,
            gateway,
            &run,
            &session,
            &mut blocks,
            &text,
            first,
            &snapshot,
        )?;

        // Never enqueue a blank reply (empty first-round content with no tool
        // call, or empty second-round content). The fallback is a fixed,
        // minimal, generic message — no product styling. The Journal still
        // records the true (possibly empty) model content for audit; only the
        // reply intent text is guarded.
        let reply_text = ensure_nonblank_reply(&llm.content);
        let intent = self.reply_intent(&run, &session, &reply_text, message_id, chat_id);
        let correlation_id = intent.invocation_id.0.clone();
        journal.append_event(
            JournalEventKind::InvocationProposed,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "operation": intent.operation,
                "idempotency_key": intent.idempotency_key,
            }),
        )?;
        let approved = gateway.approve_invocation(intent, &run, &session, &snapshot)?;
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
        self.enqueue_or_pause(
            journal,
            &approved,
            &run,
            &session,
            &correlation_id,
            &snapshot,
        )?;
        Ok(RuntimeOutcome {
            run_id: run.id,
            session_id: session.id,
            output: reply_text,
        })
    }

    pub fn deliver_echo(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        event: ValidatedEvent,
    ) -> Result<RuntimeOutcome> {
        // `deliver_echo` never calls the LLM, so the tool-recall loop does not
        // apply here — there is no model output to feed a tool result back to.
        // The loop lives only in the model-driven path (`deliver`).
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
        // Blocker 2: snapshot_id must exist and be non-empty; failure prevents Run creation.
        let snapshot_id = journal
            .current_registry_snapshot_id()
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        if snapshot_id.is_empty() {
            anyhow::bail!("registry_snapshot_invalid: snapshot ID is empty");
        }
        // Load the snapshot BEFORE creating the Run.
        let snapshot = journal
            .load_registry_snapshot(&snapshot_id)
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        let run = self.create_run(&session, &event, &snapshot_id, &snapshot);
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
            }),
        )?;
        let snap_for_gateway = snapshot;
        let RuntimeEventPayload::UserMessage {
            text,
            message_id,
            chat_id,
        } = event.payload.clone();
        let reply = format!("收到：{text}");
        let intent = self.reply_intent(&run, &session, &reply, message_id, chat_id);
        let correlation_id = intent.invocation_id.0.clone();
        journal.append_event(
            JournalEventKind::InvocationProposed,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "operation": intent.operation,
                "idempotency_key": intent.idempotency_key,
            }),
        )?;
        let approved = gateway.approve_invocation(intent, &run, &session, &snap_for_gateway)?;
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
        self.enqueue_or_pause(
            journal,
            &approved,
            &run,
            &session,
            &correlation_id,
            &snap_for_gateway,
        )?;
        Ok(RuntimeOutcome {
            run_id: run.id,
            session_id: session.id,
            output: reply,
        })
    }

    fn create_run(
        &self,
        session: &Session,
        event: &ValidatedEvent,
        snapshot_id: &str,
        snapshot: &RegistrySnapshot,
    ) -> Run {
        let now = Utc::now();
        let mut principal = event.principal.clone();
        // Add external (harness) grants from the pinned snapshot.
        // These are ReadOnly operations with BindingKind::External in the
        // snapshot that this Run is pinned to. Existing grants from the
        // validated ingress event are preserved.
        for op in &snapshot.operations {
            if op.risk == crate::registry::snapshot::Risk::ReadOnly
                && op.binding_kind == crate::registry::snapshot::BindingKind::External
                && !principal.grants.iter().any(|g| g.operation == op.name)
            {
                principal.grants.push(CapabilityGrant {
                    operation: op.name.clone(),
                    scope: "current_session".to_string(),
                });
            }
        }
        Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: self.config.agent_id.clone(),
            trigger_event_id: event.event_id.clone(),
            principal,
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
            registry_snapshot_id: snapshot_id.to_string(),
        }
    }

    fn reply_intent(
        &self,
        run: &Run,
        session: &Session,
        text: &str,
        message_id: Option<String>,
        chat_id: Option<String>,
    ) -> InvocationIntent {
        if session.channel == ChannelKind::Feishu {
            InvocationIntent {
                invocation_id: InvocationId(format!("reply:{}", run.id.0)),
                run_id: run.id.clone(),
                operation: crate::domain::operation::FEISHU_SEND_MESSAGE.to_string(),
                arguments: json!({
                    "session_id": session.id.0,
                    "message_id": message_id.unwrap_or_default(),
                    "chat_id": chat_id.unwrap_or_default(),
                    "text": text,
                }),
                idempotency_key: Some(format!("feishu-reply:{}", run.id.0)),
            }
        } else {
            InvocationIntent {
                invocation_id: InvocationId(format!("reply:{}", run.id.0)),
                run_id: run.id.clone(),
                operation: crate::domain::operation::STDOUT_SEND_TEXT.to_string(),
                arguments: json!({
                    "session_id": session.id.0,
                    "text": text,
                }),
                idempotency_key: Some(format!("stdout-reply:{}", run.id.0)),
            }
        }
    }
}

/// Return a non-blank reply string. If the model produced only whitespace
/// (empty first-round content with no tool call, or empty second-round
/// content), substitute a fixed, minimal, generic message so the Outbox never
/// receives a blank string. This is the single place reply text is synthesized.
fn ensure_nonblank_reply(content: &str) -> String {
    if content.trim().is_empty() {
        "No reply was generated for this turn.".to_string()
    } else {
        content.to_string()
    }
}
