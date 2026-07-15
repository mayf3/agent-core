use crate::config::KernelConfig;
use crate::context::ContextAssembler;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::hook::{HookClient, HookConfig};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput};
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::Result;
use serde_json::json;
pub(crate) mod coding_grants;
pub(crate) mod hook_call;
mod model_invocation;
pub mod outbox_dispatcher;
mod tool_execution;
mod tool_loop;
mod tool_rejection;
pub use crate::gateway::ToolRejection;
pub use tool_rejection::validate_model_arguments;
#[cfg(test)]
#[path = "tests/capability_probe_e2e.rs"]
mod capability_probe_e2e;
#[cfg(test)]
#[path = "tests/capability_probe_reopen.rs"]
mod capability_probe_reopen;
#[cfg(test)]
#[path = "tests/capability_probe_rollback.rs"]
mod capability_probe_rollback;
#[cfg(test)]
#[path = "tests/capability_snapshot_pin.rs"]
mod capability_snapshot_pin;
#[cfg(test)]
#[path = "tests/external_harness_failures.rs"]
mod external_harness_failures;
#[cfg(test)]
#[path = "tests/external_harness_hotload.rs"]
mod external_harness_hotload;
#[cfg(test)]
#[path = "tests/external_harness_pinning.rs"]
mod external_harness_pinning;
#[cfg(test)]
#[path = "tests/external_harness_runtime.rs"]
mod external_harness_runtime;
#[cfg(test)]
#[path = "tests/external_harness_transport.rs"]
mod external_harness_transport;
#[cfg(test)]
#[path = "tests/recall_audit.rs"]
mod recall_audit;
#[cfg(test)]
#[path = "tests/recall_isolation.rs"]
mod recall_isolation;
#[cfg(test)]
#[path = "tests/recall_security.rs"]
mod recall_security;
#[cfg(test)]
#[path = "tests/recall_test_support.rs"]
mod recall_test_support;
#[cfg(test)]
#[path = "tests/registry_snapshot_failure.rs"]
mod registry_snapshot_failure;
#[cfg(test)]
#[path = "tests/registry_snapshot_gateway.rs"]
mod registry_snapshot_gateway;
#[cfg(test)]
#[path = "tests/registry_snapshot_provider_context.rs"]
mod registry_snapshot_provider_context;
#[cfg(test)]
#[path = "tests/registry_snapshot_recovery_failure.rs"]
mod registry_snapshot_recovery_failure;
#[cfg(test)]
#[path = "tests/tool_execution_dispatch.rs"]
mod tool_execution_dispatch;
#[cfg(test)]
#[path = "tests/tool_round_budget.rs"]
mod tool_round_budget;
pub struct Runtime<L> {
    config: KernelConfig,
    llm: L,
    hook_client: Option<Box<dyn HookClient>>,
    hook_config: Option<HookConfig>,
}
pub struct RuntimeOutcome {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub output: String,
}
use hook_call::ensure_nonblank_reply;
pub use hook_call::{run_yield, session_spawn};
impl<L> Runtime<L>
where
    L: LlmClient + 'static,
{
    pub fn new(config: KernelConfig, llm: L) -> Self {
        Self {
            config,
            llm,
            hook_client: None,
            hook_config: None,
        }
    }

    /// Attach a hook client and config. When set, `context.prepare.v0` is
    /// called before each initial LLM completion.
    pub fn with_hook(mut self, client: Box<dyn HookClient>, config: HookConfig) -> Self {
        self.hook_client = Some(client);
        self.hook_config = Some(config);
        self
    }
    /// Phase 2 M2d: decide whether an approved invocation is dispatched now or
    /// paused for human approval. ReadOnly ops queue immediately; Write ops
    /// pause when require_write_approval is enabled. Risk is determined from
    /// the Run's pinned registry snapshot, not the static catalog.
    pub(crate) fn enqueue_or_pause(
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

    pub(crate) fn config(&self) -> &KernelConfig {
        &self.config
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
        let run = self.create_run(journal, &session, &event, &snapshot_id, &snapshot);
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

        // ── context.prepare.v0 hook ──────────────────────────────────────
        if let (Some(ref client), Some(ref hook_cfg)) = (&self.hook_client, &self.hook_config) {
            if hook_cfg.enabled {
                match crate::runtime::hook_call::call_context_prepare(
                    &mut blocks,
                    client.as_ref(),
                    hook_cfg,
                    journal,
                    &run.id,
                    &session.id,
                    &self.config.agent_id.0,
                    &run.principal.principal_id.0,
                    &format!("{:?}", event.source),
                    &text,
                    self.config.context_max_block_chars,
                )? {
                    crate::runtime::hook_call::HookCallOutcome::Injected { .. } => {
                        // Fragments injected successfully, Run continues.
                    }
                    crate::runtime::hook_call::HookCallOutcome::FailClosed { error } => {
                        journal.fail_run(&run.id)?;
                        journal.append_event(
                            JournalEventKind::RunFailed,
                            Some(&run.id),
                            Some(&session.id),
                            None,
                            json!({ "run_id": run.id.0, "error_category": "hook_fail_closed" }),
                        )?;
                        return self.reply_with_failure(
                            journal,
                            gateway,
                            &snapshot,
                            &run,
                            &session,
                            message_id,
                            chat_id,
                            &format!("Hook context preparation failed: {error}"),
                        );
                    }
                    crate::runtime::hook_call::HookCallOutcome::Skipped { .. } => {
                        // Run continues without hook fragments.
                    }
                }
            }
        }

        // Phase 1: initial LLM call. On failure, record RunFailed and deliver
        // a static notification (never a silent Err).
        let first = match self.complete_model_invocation(
            journal,
            &run,
            &session,
            0,
            LlmInput {
                blocks: blocks.clone(),
                user_text: text.clone(),
                granted_operations: granted_operations.clone(),
                provider_tools: provider_tools.clone(),
                follow_ups: vec![],
            },
        ) {
            Ok(llm) => llm,
            Err(_) => {
                journal.fail_run(&run.id)?;
                journal.append_event(
                    JournalEventKind::RunFailed,
                    Some(&run.id),
                    Some(&session.id),
                    None,
                    json!({ "run_id": run.id.0, "error_category": "initial_llm_failed" }),
                )?;
                return self.reply_with_failure(
                    journal,
                    gateway,
                    &snapshot,
                    &run,
                    &session,
                    message_id,
                    chat_id,
                    crate::runtime::tool_loop::INITIAL_LLM_FAILED_MSG,
                );
            }
        };
        // Phase 2: tool recall loop. Follow-up LLM failures are handled
        // internally (tool_loop::handle_followup_llm_failure records RunFailed
        // and returns a static failure LlmOutput).
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
        // error), enqueue the reply without changing status. Otherwise use the
        // normal enqueue_or_pause path.
        let reply_text = ensure_nonblank_reply(&llm.content);
        let is_failed = matches!(
            journal.run_status(&run.id),
            Ok(Some(s)) if s == "Failed"
        );
        if is_failed {
            return self.reply_with_failure(
                journal,
                gateway,
                &snapshot,
                &run,
                &session,
                message_id,
                chat_id,
                &reply_text,
            );
        }
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
}
