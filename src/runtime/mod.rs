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
mod coding_task_tool;
pub(crate) mod hook_call;
mod model_invocation;
pub mod outbox_dispatcher;
mod tool_execution;
mod tool_loop;
mod tool_rejection;
pub use crate::gateway::ToolRejection;
pub use tool_rejection::validate_model_arguments;
pub(crate) use tool_loop::ToolCallOutcome;
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
#[path = "tests/continuation_worker_idempotent.rs"]
mod continuation_worker_idempotent;
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
#[path = "tests/run_budget.rs"]
mod run_budget;
#[cfg(test)]
#[path = "tests/run_budget_closeout.rs"]
mod run_budget_closeout;
#[cfg(test)]
#[path = "tests/tool_execution_dispatch.rs"]
mod tool_execution_dispatch;
#[cfg(test)]
#[path = "tests/tool_round_budget/mod.rs"]
mod tool_round_budget;
pub struct Runtime<L> {
    config: KernelConfig,
    llm: L,
    hook_client: Option<Box<dyn HookClient>>,
    hook_config: Option<HookConfig>,
    budget_hook_config: Option<HookConfig>,
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
            budget_hook_config: None,
        }
    }

    /// Attach a hook client and config. When set, `context.prepare.v0` is
    /// called before every initial and follow-up LLM completion.
    pub fn with_hook(mut self, client: Box<dyn HookClient>, config: HookConfig) -> Self {
        self.hook_client = Some(client);
        self.hook_config = Some(config);
        self
    }

    /// Attach a budget hook config. The same hook client is reused. When set
    /// and enabled, `run.budget.resolve.v0` is called once at Run creation
    /// to resolve the Run's frozen budget.
    pub fn with_budget_hook(mut self, config: HookConfig) -> Self {
        self.budget_hook_config = Some(config);
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
        // Resolve and freeze the Run's budget before inserting the Run so a
        // fail-closed budget hook failure prevents Run creation.
        let budget = match self.resolve_run_budget(journal, &run, &session, &snapshot) {
            Ok(b) => b,
            Err(e) => {
                // Budget hook failed closed — the Run cannot start.
                return Err(e);
            }
        };
        let run = Run {
            budget_hook_id: Some(budget.hook_id.clone()),
            budget_hook_version: Some(budget.hook_version.clone()),
            budget_decision_digest: Some(budget.decision.digest()),
            budget_max_tool_rounds: Some(budget.decision.max_tool_rounds),
            budget_max_wall_time_ms: Some(budget.decision.max_wall_time_ms),
            budget_exhaustion_action: Some(budget.decision.exhaustion_action),
            ..run
        };
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

        // Run wall-clock deadline (High 2): frozen once at Run start. The
        // initial model call and every tool-loop round carry the remaining
        // budget into the transport so nothing outlives the deadline.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(
                run.budget_max_wall_time_ms
                    .unwrap_or(self.config.tool_loop_timeout_ms),
            );

        // Phase 1: initial LLM call. On failure, record RunFailed and deliver
        // a static notification (never a silent Err).
        let first = match self.complete_model_invocation(
            journal,
            &run,
            &session,
            0,
            LlmInput {
                timeout_override_ms: None,
                blocks: blocks.clone(),
                user_text: text.clone(),
                granted_operations: granted_operations.clone(),
                provider_tools: provider_tools.clone(),
                follow_ups: vec![],
            },
            Some(deadline),
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
        // High 3: when the Run ended with a budget yield, record ONLY the
        // structured yield fact — never fabricate a "请发送继续" user reply,
        // never create a reply Invocation, never enter the outbox. The
        // external Agent Loop Harness observes the yield fact and decides
        // whether to continue.
        if journal.run_yielded(&run.id)? {
            journal.complete_run(&run.id)?;
            return Ok(RuntimeOutcome {
                run_id: run.id,
                session_id: session.id,
                output: String::new(),
            });
        }
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
        let mut intent = self.reply_intent(&run, &session, &reply_text, message_id, chat_id);
        apply_pending_proposal_presentation(journal, &run, &mut intent)?;
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
    /// Same-session continuation scheduling (Bootstrap V0, High 2 + High 1).
    ///
    /// Called by the worker for a `schedule_continuation` job after an
    /// authorized external Agent Loop Harness requested the next Run in the
    /// SAME session based on a trigger Run. This is NOT a user message:
    /// no `IngressAccepted`, no `RuntimeEventPayload::UserMessage`, no fake
    /// "user: 继续" — the model continues from the session's accumulated
    /// context (prior turns, tool results, compaction).
    ///
    /// High 1: the next Run INHERITS the trigger Run's FROZEN governance
    /// facts — `trigger.agent_id`, `trigger.registry_snapshot_id` (and the
    /// fixed Registry Snapshot loaded from it), and the already-frozen
    /// `trigger.principal` / grants. Current KernelConfig / current snapshot
    /// changes never affect a continuation. `next_run_id` is PRE-ALLOCATED in
    /// the continuation ledger (High 4) and used as-is; the worker never
    /// generates a Run id itself.
    pub(crate) fn schedule_run_for_existing_session(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        trigger: &Run,
        session: &Session,
        trigger_run_id: &RunId,
        next_run_id: &RunId,
    ) -> Result<RuntimeOutcome> {
        // High 1: the fixed Registry Snapshot pinned by the trigger Run —
        // never the current one.
        if trigger.registry_snapshot_id.is_empty() {
            anyhow::bail!("trigger_run_registry_snapshot_invalid");
        }
        let snapshot = journal
            .load_registry_snapshot(&trigger.registry_snapshot_id)
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        // High 1 (defense in depth): the trigger Run's frozen governance facts
        // must be consistent with the Session the worker loaded from
        // `trigger.session_id`. The gateway already enforces this at acceptance,
        // but the worker re-reads the journal independently, so it re-verifies
        // the same generic facts before touching any Run. Any mismatch fails
        // closed: no Run, no RunStarted, no model call.
        if trigger.session_id != session.id {
            anyhow::bail!("continuation_session_mismatch");
        }
        if trigger.agent_id != session.agent_id {
            anyhow::bail!("continuation_agent_mismatch");
        }
        if !matches!(
            (&trigger.principal.source, &session.channel),
            (PrincipalSource::Cli, ChannelKind::Cli)
                | (PrincipalSource::Feishu, ChannelKind::Feishu)
        ) {
            anyhow::bail!("continuation_principal_mismatch");
        }
        // High 4: worker idempotency based on the Run's LIFECYCLE facts. This
        // is FAIL CLOSED — no automatic recovery, no model/tool replay, no
        // checkpoint, no compensation. The pre-allocated Run may already be
        // present from a prior attempt:
        //   - row missing                     → create it + RunStarted
        //                                       atomically (the ONLY happy path);
        //   - row present, terminal state     → return the existing outcome;
        //   - row present, no RunStarted      → fail closed (pre-fix crash window
        //                                       or a corrupted DB) — never
        //                                       auto-recover, never re-execute;
        //   - row present, RunStarted, no     → fail closed / stranded — do NOT
        //     terminal state                    re-invoke model/tools, do NOT fake
        //                                       success;
        //   - row present, conflicting facts  → fail closed (never overwrite).
        if let Some(existing) = journal.run_by_id(next_run_id)? {
            // Conflicting facts on the SAME pre-allocated run_id always fail
            // closed — never overwrite, never execute.
            if existing.session_id != session.id
                || existing.agent_id != session.agent_id
                || existing.registry_snapshot_id != trigger.registry_snapshot_id
            {
                anyhow::bail!("continuation_run_conflict");
            }
            let started = journal.run_has_started(next_run_id)?;
            match (&existing.status, started) {
                // Already reached a genuine terminal state — return it as-is.
                (RunStatus::Completed, _) | (RunStatus::Failed, _) => {
                    return Ok(RuntimeOutcome {
                        run_id: existing.id,
                        session_id: session.id.clone(),
                        output: String::new(),
                    });
                }
                // RunStarted exists but the Run never converged to a terminal
                // state — it is stranded mid-execution. Fail closed; do NOT
                // re-invoke the model or tools, and do NOT pretend success.
                (_, true) => {
                    journal.fail_run(&existing.id)?;
                    journal.append_event(
                        JournalEventKind::RunFailed,
                        Some(&existing.id),
                        Some(&session.id),
                        None,
                        json!({
                            "run_id": existing.id.0,
                            "error_category": "continuation_run_stranded",
                            "reason": "RunStarted exists but no terminal state",
                        }),
                    )?;
                    anyhow::bail!("continuation_run_stranded");
                }
                // Run row exists but RunStarted was never written (the pre-fix
                // crash window, or a corrupted DB). Fail closed — do NOT
                // auto-recover, do NOT re-execute. Report the anomaly.
                (RunStatus::Running, false)
                | (RunStatus::WaitingDispatch, false)
                | (RunStatus::Unknown, false)
                | (RunStatus::AwaitingApproval, false) => {
                    journal.fail_run(&existing.id)?;
                    journal.append_event(
                        JournalEventKind::RunFailed,
                        Some(&existing.id),
                        Some(&session.id),
                        None,
                        json!({
                            "run_id": existing.id.0,
                            "error_category": "continuation_run_partial",
                            "reason": "Run row exists but RunStarted missing",
                        }),
                    )?;
                    anyhow::bail!("continuation_run_partial");
                }
            }
        }
        // The continuation is a NEW governance event: it is NOT the trigger
        // Run's event and NOT a user message. It only carries the fact that an
        // authorized caller asked for the next Run in the same session.
        let continuation_event_id = EventId::new();
        let run = self.create_run_frozen(
            session,
            trigger,
            &continuation_event_id,
            next_run_id,
            &trigger.registry_snapshot_id,
        );
        let budget = match self.resolve_run_budget(journal, &run, session, &snapshot) {
            Ok(b) => b,
            Err(e) => {
                // Budget hook failed closed — the Run cannot start.
                return Err(e);
            }
        };
        let run = Run {
            budget_hook_id: Some(budget.hook_id.clone()),
            budget_hook_version: Some(budget.hook_version.clone()),
            budget_decision_digest: Some(budget.decision.digest()),
            budget_max_tool_rounds: Some(budget.decision.max_tool_rounds),
            budget_max_wall_time_ms: Some(budget.decision.max_wall_time_ms),
            budget_exhaustion_action: Some(budget.decision.exhaustion_action),
            ..run
        };
        // High 4: Run row + RunStarted are written in ONE transaction so the
        // "row exists but RunStarted missing" crash window cannot occur for a
        // freshly created Run.
        journal.insert_run_and_start(
            &run,
            &session.id,
            &continuation_event_id.0,
            trigger_run_id,
        )?;

        let granted_operations: Vec<String> = run
            .principal
            .grants
            .iter()
            .map(|g| g.operation.clone())
            .collect();
        let mut blocks = ContextAssembler::from_config(&self.config).build_continuation(
            journal,
            session,
            &continuation_event_id.0,
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
        let provider_tools = snapshot.provider_tools_for_grants(&granted_operations);
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(
                run.budget_max_wall_time_ms
                    .unwrap_or(self.config.tool_loop_timeout_ms),
            );
        let first = match self.complete_model_invocation(
            journal,
            &run,
            session,
            0,
            LlmInput {
                timeout_override_ms: None,
                blocks: blocks.clone(),
                user_text: String::new(),
                granted_operations: granted_operations.clone(),
                provider_tools: provider_tools.clone(),
                follow_ups: vec![],
            },
            Some(deadline),
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
                    session,
                    None,
                    None,
                    crate::runtime::tool_loop::INITIAL_LLM_FAILED_MSG,
                );
            }
        };
        let llm = self.run_tool_recall_loop(
            journal,
            gateway,
            &run,
            session,
            &mut blocks,
            "",
            first,
            &snapshot,
        )?;
        // Yield: record the structured fact only, never fabricate a user
        // reply, never create a reply Invocation, never enter the outbox.
        if journal.run_yielded(&run.id)? {
            journal.complete_run(&run.id)?;
            return Ok(RuntimeOutcome {
                run_id: run.id,
                session_id: session.id.clone(),
                output: String::new(),
            });
        }
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
                session,
                None,
                None,
                &reply_text,
            );
        }
        let mut intent = self.reply_intent(&run, session, &reply_text, None, None);
        apply_pending_proposal_presentation(journal, &run, &mut intent)?;
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
        let approved = gateway.approve_invocation(intent, &run, session, &snapshot)?;
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
            session,
            &correlation_id,
            &snapshot,
        )?;
        Ok(RuntimeOutcome {
            run_id: run.id,
            session_id: session.id.clone(),
            output: reply_text,
        })
    }
}

fn apply_pending_proposal_presentation(
    journal: &JournalStore,
    run: &Run,
    intent: &mut InvocationIntent,
) -> Result<()> {
    if intent.operation != crate::domain::operation::FEISHU_SEND_MESSAGE {
        return Ok(());
    }
    if let Some(proposal_id) = journal.pending_capability_proposal_for_run(&run.id)? {
        if let Some(arguments) = intent.arguments.as_object_mut() {
            arguments.remove("text");
            arguments.insert(
                "presentation".into(),
                json!({"kind":"capability_proposal_pending_v1","proposal_id":proposal_id}),
            );
        }
    }
    Ok(())
}
