//! THE DEFAULT Runtime: every message is delivered through the
//! standalone `agent-runtime` crate. The legacy Runtime is frozen and
//! reachable only through the explicit emergency flag
//! `AGENT_CORE_FORCE_LEGACY_RUNTIME` (a short-term escape hatch and
//! comparison path, not a routing framework).
//!
//! Host-side wiring ONLY. This module knows the legacy Kernel intimately
//! (journal, gateway, run lifecycle, reply path) — the new Runtime knows
//! none of it. The split is:
//!
//! ```text
//! agent-runtime (RuntimeLoop)
//!   ↓ narrow Model / InvocationPort
//! this host adapter
//!   ↓ legacy Kernel
//! journal / invoke_tool / reply path / Provider
//! ```
//!
//! Everything the new Runtime must not know (run_id, turn/tool position,
//! remaining timeout, session/run objects) is remembered HERE, host-side.
//! `run_id` remains LEGACY COMPATIBILITY DEBT — the Runtime -> run_id ->
//! Kernel shape is NOT the final V2 Kernel boundary.
//!
//! NO identity selection: future multi-agent routing belongs to an
//! external Router Harness, not this module. On failure this path NEVER
//! falls back to the legacy Runtime for the same message — the Run is
//! failed and the message is done.

use crate::config::KernelConfig;
use crate::domain::{
    JournalEventKind, Run, RunId, RunPrincipal, RuntimeEventPayload, Session, ValidatedEvent,
};
use crate::gateway::Gateway;
use crate::hook::{HookClient, HookConfig, HookEndpoint, HookKind};
use crate::journal::JournalStore;
use crate::llm::{
    LlmClient, LlmFollowUp, LlmInput, ProviderToolTurn, ToolCall, ToolCallResult,
};
use crate::runtime::hook_call::ensure_nonblank_reply;
use crate::runtime::{Runtime, ToolCallOutcome};
use agent_runtime::{
    Action, InvocationPort, InvocationResult, InvocationStatus, Model, ModelOutput, RuntimeLoop,
    Tool, Turn,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Deliver one validated user message through the standalone agent-runtime.
///
/// Session/Run lifecycle, tool execution, and the reply path are all
/// borrowed from the legacy host: the new Runtime only sees the model, the
/// invocation port, and the tool view. On ANY failure the Run is marked
/// Failed and the error propagates — the legacy Runtime is never invoked
/// for this message.
pub(crate) fn deliver_via_runtime_v0(
    config: &KernelConfig,
    journal: &JournalStore,
    gateway: &Gateway,
    validated: ValidatedEvent,
) -> Result<()> {
    let session = journal.get_or_create_session(&validated.session_target)?;
    journal.append_event(
        JournalEventKind::SessionReady,
        None,
        Some(&session.id),
        Some(&validated.event_id.0),
        json!({
            "session_id": session.id.0,
            "agent_id": session.agent_id.0,
            "channel": format!("{:?}", session.channel),
            "conversation_key": session.conversation_key,
        }),
    )?;
    let snapshot_id = journal
        .current_registry_snapshot_id()
        .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
    if snapshot_id.is_empty() {
        bail!("registry_snapshot_invalid: snapshot ID is empty");
    }
    let snapshot = journal
        .load_registry_snapshot(&snapshot_id)
        .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;

    // Host side keeps the legacy Run lifecycle intact: create -> execute ->
    // complete on success / fail on error. Never a dangling Run.
    let runtime_llm: Box<dyn LlmClient> =
        Box::new(super::delivery::build_llm_from_config(config));
    let runtime = Runtime::new(config.clone(), runtime_llm);
    let run = runtime.create_run(journal, &session, &validated, &snapshot_id, &snapshot);
    journal.insert_run(&run)?;
    journal.append_event(
        JournalEventKind::RunStarted,
        Some(&run.id),
        Some(&session.id),
        Some(&validated.event_id.0),
        json!({
            "run_id": run.id.0,
            "trigger_event_id": run.trigger_event_id.0,
            "principal_id": run.principal.principal_id.0,
        }),
    )?;

    let granted_operations: Vec<String> = run
        .principal
        .grants
        .iter()
        .map(|g| g.operation.clone())
        .collect();
    let provider_tools = snapshot.provider_tools_for_grants(&granted_operations);
    let tool_view = to_runtime_tools(&provider_tools);

    let (text, message_id, chat_id) = match validated.payload.clone() {
        RuntimeEventPayload::UserMessage {
            text,
            message_id,
            chat_id,
        } => (text, message_id, chat_id),
    };

    // ContextPrepareV0: when configured, the external Context Harness can
    // add the Agent workspace's AGENTS.md instruction to every model round.
    let hook_client: Option<Box<dyn HookClient>> = if config.context_prepare_hook.enabled {
        Some(Box::new(crate::hook::HttpHookClient::new()))
    } else {
        None
    };
    let model = RuntimeV0Model::new(
        Box::new(super::delivery::build_llm_from_config(config)),
        provider_tools,
        snapshot.clone(),
        hook_client,
        Some(config.context_prepare_hook.clone()),
        journal,
        run.clone(),
        session.clone(),
    );
    let port = RuntimeV0InvocationAdapter {
        runtime: &runtime,
        journal,
        gateway,
        run_id: run.id.clone(),
        tool_call_count: std::cell::Cell::new(0),
        remaining_ms: config.tool_loop_timeout_ms,
    };
    // The bounded working loop: the model may keep calling tools, but at
    // most `max_tool_rounds` REAL executions per Run. The new Runtime only
    // receives this plain integer — no budget, no deadline, no yield.
    let mut loop_runtime =
        RuntimeLoop::with_max_tool_calls(port, model, config.max_tool_rounds);
    let reply = match loop_runtime.run(&text, &tool_view) {
        Ok(reply) => reply,
        Err(error) => {
            // Fail the Run; NEVER re-run the legacy Runtime for this message
            // (the new Runtime may already have executed a real tool).
            journal.fail_run(&run.id)?;
            journal.append_event(
                JournalEventKind::RunFailed,
                Some(&run.id),
                Some(&session.id),
                None,
                json!({
                    "run_id": run.id.0,
                    "error_category": "runtime_v0_failed",
                    "reason": error,
                }),
            )?;
            bail!("runtime_v0_failed: {error}");
        }
    };

    // Reuse the legacy reply mechanism: propose -> approve -> enqueue. No
    // new Reply framework. Any failure here also fails the Run — never a
    // dangling Run, never a legacy fallback.
    let reply_result: Result<()> = (|| {
        let reply_text = ensure_nonblank_reply(&reply);
        let intent = runtime.reply_intent(&run, &session, &reply_text, message_id, chat_id);
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
        runtime.enqueue_or_pause(
            journal,
            &approved,
            &run,
            &session,
            &correlation_id,
            &snapshot,
        )?;
        Ok(())
    })();
    if let Err(error) = reply_result {
        journal.fail_run(&run.id)?;
        journal.append_event(
            JournalEventKind::RunFailed,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({
                "run_id": run.id.0,
                "error_category": "runtime_v0_reply_failed",
                "reason": error.to_string(),
            }),
        )?;
        return Err(error);
    }
    journal.complete_run(&run.id)?;
    Ok(())
}

/// Convert the Kernel's pre-computed provider tool definitions into the
/// new Runtime's tool view — name, description, parameters schema only.
/// No ToolView service/framework. The operation-name ↔ capability mapping
/// stays LEGACY COMPATIBILITY DEBT.
fn to_runtime_tools(provider_tools: &[Value]) -> Vec<Tool> {
    provider_tools
        .iter()
        .map(|t| Tool {
            name: t
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            description: t
                .pointer("/function/description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            parameters: t
                .pointer("/function/parameters")
                .cloned()
                .unwrap_or(json!({"type": "object"})),
        })
        .collect()
}

/// Minimal generic coding behaviour guidance injected as the runtime
/// system block. Deliberately tiny — not a prompt framework, no Route
/// Harness specifics.
const CODING_INSTRUCTION: &str = "\
# Coding 工作说明
- 完整代码开发任务优先使用高层 Coding Workspace Task（external.coding_workspace_task）；
- read / exec 用于调查和验证，不要逐文件无休止探索；
- 搜索优先于逐个文件读取；
- 同类不可恢复错误重复出现时停止；
- 开发结果必须检查 git diff；
- 运行相关测试；
- 未完成时明确报告 blocker，不机械重试。";

/// Thin adapter wrapping the production LLM as `agent_runtime::Model`.
///
/// Only the minimal conversions: turn -> LlmInput (no full ContextAssembler,
/// no history, no memory, no compaction) and LlmOutput -> ModelOutput. The
/// provider transcripts of EVERY prior tool round are remembered here,
/// host-side, so a later model turn can replay all accumulated tool results
/// as proper provider tool turns.
///
/// When a ContextPrepareV0 hook is configured, every model round runs
/// through the existing stage -> hook -> materialize -> complete chain so
/// the external Context Harness can add the Agent workspace's AGENTS.md
/// instruction. Hook failure degrades to the candidate (fail-open); the
/// built-in CODING_INSTRUCTION stays as the fallback.
struct RuntimeV0Model<'a> {
    llm: Box<dyn LlmClient>,
    tools_json: Vec<Value>,
    /// The Run's PINNED registry snapshot — the ONLY authority for the
    /// context.prepare.v0 hook binding (provider / endpoint). Env never
    /// picks the provider.
    snapshot: std::sync::Arc<crate::registry::snapshot::RegistrySnapshot>,
    /// Provider transcript of each executed tool round, in execution order.
    /// Index `i` pairs with `turn.follow_ups[i]`.
    provider_turn_history: Vec<ProviderToolTurn>,
    hook_client: Option<Box<dyn HookClient>>,
    hook_config: Option<HookConfig>,
    journal: &'a JournalStore,
    run: Run,
    session: Session,
    round_counter: std::cell::Cell<usize>,
}

impl<'a> RuntimeV0Model<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        llm: Box<dyn LlmClient>,
        tools_json: Vec<Value>,
        snapshot: std::sync::Arc<crate::registry::snapshot::RegistrySnapshot>,
        hook_client: Option<Box<dyn HookClient>>,
        hook_config: Option<HookConfig>,
        journal: &'a JournalStore,
        run: Run,
        session: Session,
    ) -> Self {
        Self {
            llm,
            tools_json,
            snapshot,
            provider_turn_history: vec![],
            hook_client,
            hook_config,
            journal,
            run,
            session,
            round_counter: std::cell::Cell::new(0),
        }
    }
}
/// The Run's pinned snapshot is the ONLY authority for the
/// context.prepare.v0 hook binding. A missing/ambiguous binding or a
/// Builtin binding (no builtin context provider exists) yields None — the
/// hook does NOT run, and env is never consulted for a provider.
fn context_hook_binding(
    snapshot: &crate::registry::snapshot::RegistrySnapshot,
) -> Option<&crate::registry::snapshot::HookBinding> {
    let binding = snapshot.hook_binding(crate::registry::snapshot::CONTEXT_PREPARE_HOOK_CONTRACT)?;
    if binding.binding_kind != crate::registry::snapshot::BindingKind::External {
        return None;
    }
    Some(binding)
}

/// The local credential's provider identity must match the binding's
/// provider (fail closed on mismatch, mirroring the budget hook path).
fn validate_context_credential(
    binding: &crate::registry::snapshot::HookBinding,
    local: &HookConfig,
) -> Result<(), String> {
    if local.provider_id != binding.provider_id {
        return Err(format!(
            "context_hook_credential_mismatch: binding provider {} != local credential provider {}",
            binding.provider_id, local.provider_id
        ));
    }
    Ok(())
}

/// Provider/endpoint come from the binding; env keeps only the LOCAL call
/// mechanics (credential, timeout, request limits, failure policy).
fn build_context_hook_config(
    binding: &crate::registry::snapshot::HookBinding,
    local: &HookConfig,
) -> HookConfig {
    HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: binding.endpoint.clone(),
        },
        timeout_ms: local.timeout_ms,
        max_request_bytes: local.max_request_bytes,
        max_response_bytes: local.max_response_bytes,
        failure_mode: local.failure_mode,
        provider_id: binding.provider_id.clone(),
        shared_secret: local.shared_secret.clone(),
        ..Default::default()
    }
}

impl<'a> RuntimeV0Model<'a> {
    /// Run the base input through the existing ContextPrepareV0 chain:
    /// stage_candidate -> context hook -> materialize. The provider may ADD
    /// Agent instruction blocks (e.g. AGENTS.md via the Context Harness);
    /// user_text, tools and original blocks stay as the provider returned
    /// them. Hook absence/failure degrades to the unmodified candidate
    /// (fail-open); fail-closed configuration terminates.
    fn prepare_input(&mut self, input: LlmInput) -> Result<LlmInput, String> {
        let candidate = self
            .llm
            .stage_candidate(input.clone())
            .map_err(|e| format!("context_stage_failed: {e}"))?;
        let round_index = self.round_counter.get();
        self.round_counter.set(round_index + 1);
        // The Run's pinned snapshot is the ONLY authority for the
        // context.prepare.v0 binding; env never picks the provider.
        let Some(binding) = context_hook_binding(&self.snapshot) else {
            return Ok(input);
        };
        let Some(local) = self.hook_config.as_ref() else {
            return Ok(input);
        };
        validate_context_credential(binding, local)?;
        let hook_config = build_context_hook_config(binding, local);
        let outcome = crate::runtime::hook_call::call_context_artifact_hook(
            &candidate,
            self.hook_client.as_deref(),
            Some(&hook_config),
            self.journal,
            &self.run,
            &self.session,
            round_index,
        )
        .map_err(|e| format!("context_hook_failed: {e}"))?;
        match outcome {
            crate::runtime::hook_call::ContextArtifactOutcome::Provider { artifacts, .. } => {
                match self
                    .llm
                    .materialize_context(&candidate, &artifacts)
                    .map_err(|e| format!("context_materialize_failed: {e}"))?
                {
                    crate::llm::ModelMaterialization::Ready { input, .. } => Ok(input),
                    crate::llm::ModelMaterialization::OverBudget { .. } => {
                        Err("context_over_budget".to_string())
                    }
                }
            }
            // Not configured / disabled / provider failed (fail-open):
            // use the unmodified candidate.
            crate::runtime::hook_call::ContextArtifactOutcome::Candidate { .. } => Ok(input),
            crate::runtime::hook_call::ContextArtifactOutcome::Terminate { error_code } => {
                Err(format!("context_hook_terminated:{error_code}"))
            }
        }
    }
}

impl Model for RuntimeV0Model<'_> {
    fn complete(&mut self, turn: &Turn) -> Result<ModelOutput, String> {
        // Every accumulated tool result is replayed to the model with its
        // OWN provider transcript, so round N sees all prior results A..N-1.
        let follow_ups: Vec<LlmFollowUp> = turn
            .follow_ups
            .iter()
            .enumerate()
            .map(|(i, fr)| {
                let provider_turn = self.provider_turn_history.get(i).cloned().ok_or_else(|| {
                    format!("missing_provider_turn_for_follow_up_{i}")
                })?;
                Ok(LlmFollowUp {
                    provider_turn,
                    result_content: fr.text.clone(),
                })
            })
            .collect::<Result<_, String>>()?;
        let input = LlmInput {
            blocks: vec![crate::domain::ContextBlock {
                kind: crate::domain::ContextBlockKind::RootSystem,
                content: CODING_INSTRUCTION.to_string(),
                source_ref: None,
            }],
            user_text: turn.user_text.clone(),
            granted_operations: vec![],
            provider_tools: self.tools_json.clone(),
            follow_ups,
            timeout_override_ms: None,
        };
        let prepared = self.prepare_input(input)?;
        let output = self.llm.complete(prepared).map_err(|e| e.to_string())?;
        if let Some(provider_turn) = output.provider_turn.clone() {
            self.provider_turn_history.push(provider_turn);
        }
        match output.tool_call {
            ToolCallResult::Valid(call) => Ok(ModelOutput {
                text: output.content,
                action: Some(Action {
                    tool: call.operation,
                    arguments: call.arguments,
                }),
            }),
            ToolCallResult::Absent => Ok(ModelOutput {
                text: output.content,
                action: None,
            }),
            ToolCallResult::Malformed(_) => Err("malformed_tool_call_from_model".to_string()),
        }
    }
}

/// Host-side InvocationPort: translates the new Runtime's narrow submit
/// into the legacy `invoke_tool` seam. All legacy bookkeeping (run_id,
/// turn/tool position, remaining timeout) lives here and never enters the
/// new Runtime's API.
struct RuntimeV0InvocationAdapter<'a> {
    runtime: &'a Runtime<Box<dyn LlmClient>>,
    journal: &'a JournalStore,
    gateway: &'a Gateway,
    run_id: RunId,
    /// Monotonic counter of REAL tool actions submitted through this
    /// adapter. Each action maps to a distinct legacy turn position
    /// (0, 1, 2, ...), matching the legacy `tool:{run}:{turn}:{tool}:{id}`
    /// idempotency-key shape — consecutive actions never collide.
    tool_call_count: std::cell::Cell<usize>,
    remaining_ms: u64,
}

impl InvocationPort for RuntimeV0InvocationAdapter<'_> {
    fn submit(
        &self,
        invocation_id: &str,
        capability_ref: &str,
        arguments: Value,
    ) -> Result<InvocationResult, String> {
        let turn_index = self.tool_call_count.get();
        self.tool_call_count.set(turn_index + 1);
        let tool_call = ToolCall {
            id: invocation_id.to_string(),
            operation: capability_ref.to_string(),
            arguments,
        };
        let outcome = self
            .runtime
            .invoke_tool(
                self.journal,
                self.gateway,
                &self.run_id,
                &tool_call,
                turn_index,
                0, // one action per model round on this path
                self.remaining_ms,
            )
            .map_err(|e| format!("tool_execution_failed: {e}"))?;
        match outcome {
            ToolCallOutcome::ToolResult { text } => Ok(InvocationResult {
                invocation_id: invocation_id.to_string(),
                status: InvocationStatus::Succeeded,
                output: json!(text),
            }),
            ToolCallOutcome::Fatal { category } => Err(format!("fatal: {category}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, ChannelKind, EventId, EventSource, SessionTarget};
    use crate::journal::JournalStore;
    use chrono::Utc;
    use uuid::Uuid;

    fn principal_with_id(id: &str) -> RunPrincipal {
        RunPrincipal {
            principal_id: crate::domain::PrincipalId(id.to_string()),
            subject: crate::domain::PrincipalSubject::LocalUser,
            source: crate::domain::PrincipalSource::Cli,
            // Mirror the gateway ingress shape: the channel baseline grants
            // are part of the principal before the event reaches delivery.
            grants: crate::domain::operation::ExecutionProfile::for_channel(ChannelKind::Cli)
                .grants,
            requester_id: None,
        }
    }

    /// A principal WITHOUT the channel baseline grants — used to inject a
    /// deterministic failure on the canary path (the reply approval cannot
    /// be granted).
    fn principal_without_grants(id: &str) -> RunPrincipal {
        RunPrincipal {
            principal_id: crate::domain::PrincipalId(id.to_string()),
            subject: crate::domain::PrincipalSubject::LocalUser,
            source: crate::domain::PrincipalSource::Cli,
            grants: vec![],
            requester_id: None,
        }
    }

    fn validated_cli_event(text: &str) -> ValidatedEvent {
        ValidatedEvent {
            event_id: EventId::new(),
            source: EventSource::Cli,
            principal: principal_with_id("cli:local"),
            session_target: SessionTarget {
                agent_id: AgentId("main".into()),
                channel: ChannelKind::Cli,
                conversation_key: "canary-test".into(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text: text.to_string(),
                message_id: None,
                chat_id: None,
            },
            dedupe_key: format!("dedupe_{}", Uuid::new_v4().simple()),
            occurred_at: Utc::now(),
            chat_type: None,
        }
    }

    /// End-to-end host wiring with the switch ON: the message goes through
    /// the new Runtime (no ContextBuilt from the legacy assembler), the
    /// Run completes, and the reply invocation is queued through the
    /// legacy reply path.
    #[test]
    fn new_runtime_is_default_delivers_through_new_runtime() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let config = KernelConfig::from_cli(None); // no switch set: new Runtime is the default
        let gateway = Gateway::new(config.clone());

        super::super::delivery::deliver_event(config, &journal, &gateway, validated_cli_event("hello"))?;

        let events = journal.events()?;
        // New path: no legacy ContextAssembler, no ContextBuilt.
        assert!(
            !events.iter().any(|e| e.kind == JournalEventKind::ContextBuilt),
            "legacy context assembly must not run on the canary path"
        );
        let run_id = events
            .iter()
            .find(|e| e.kind == JournalEventKind::RunStarted)
            .and_then(|e| e.run_id.clone())
            .expect("RunStarted must be recorded");
        // Run terminates cleanly — no dangling state.
        assert_eq!(
            journal.run_status(&run_id)?,
            Some("Completed".to_string()),
            "canary Run must complete"
        );
        // Reply went through the legacy approve + outbox path.
        assert!(
            events.iter().any(|e| e.kind == JournalEventKind::InvocationProposed),
            "reply must be proposed"
        );
        assert!(
            events.iter().any(|e| e.kind == JournalEventKind::InvocationApproved),
            "reply must be approved"
        );
        let outbox = journal.outbox_dispatch_status_counts()?;
        let queued: i64 = outbox.values().sum();
        assert!(
            queued > 0,
            "reply invocation must be queued for dispatch, got: {outbox:?}"
        );
        Ok(())
    }

    /// The EMERGENCY escape hatch: AGENT_CORE_FORCE_LEGACY_RUNTIME=true
    /// routes through the legacy Runtime (ContextBuilt appears) while the
    /// frozen legacy path is kept reachable.
    #[test]
    fn force_legacy_runtime_uses_legacy_runtime() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let mut config = KernelConfig::from_cli(None);
        config.force_legacy_runtime = true;
        let gateway = Gateway::new(config.clone());

        super::super::delivery::deliver_event(config, &journal, &gateway, validated_cli_event("hello"))?;

        let events = journal.events()?;
        assert!(
            events.iter().any(|e| e.kind == JournalEventKind::ContextBuilt),
            "explicit force-legacy must run the legacy ContextAssembler"
        );
        Ok(())
    }

    /// Failure on the new path must NOT fall back to the legacy Runtime:
    /// with the switch ON, a principal without channel grants reaches the
    /// reply approval, which fails deterministically. The Run is marked
    /// Failed, the delivery errors, and no second Run is ever created — the
    /// legacy Runtime is never invoked for the same message.
    #[test]
    fn new_runtime_failure_never_falls_back_to_legacy_runtime() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let config = KernelConfig::from_cli(None); // default path: new Runtime
        let gateway = Gateway::new(config.clone());

        let mut event = validated_cli_event("hi");
        event.principal = principal_without_grants("cli:local");
        let result = super::super::delivery::deliver_event(config, &journal, &gateway, event);
        assert!(result.is_err(), "canary path must fail");

        assert_eq!(
            journal.run_count()?,
            1,
            "exactly ONE Run — the legacy Runtime must never re-run this message"
        );
        let run_id = journal
            .events()?
            .iter()
            .find(|e| e.kind == JournalEventKind::RunStarted)
            .and_then(|e| e.run_id.clone())
            .expect("RunStarted must exist");
        assert_eq!(
            journal.run_status(&run_id)?,
            Some("Failed".to_string()),
            "the canary Run must be marked Failed, not left dangling"
        );
        Ok(())
    }

    /// A recording fake LlmClient: remembers the tool-result contents of
    /// every call and scripted as: tool call → tool call → final answer.
    struct RecordingLlm {
        seen: std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>,
        rounds: std::cell::Cell<u32>,
    }

    impl LlmClient for RecordingLlm {
        fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
            self.seen.borrow_mut().push(
                input
                    .follow_ups
                    .iter()
                    .map(|f| f.result_content.clone())
                    .collect(),
            );
            let round = self.rounds.get();
            self.rounds.set(round + 1);
            let (tool_call, provider_turn) = match round {
                0 => (
                    crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                        id: "tc_1".into(),
                        operation: "system.status".into(),
                        arguments: json!({}),
                    }),
                    Some(crate::llm::ProviderToolTurn {
                        endpoint: crate::llm::EndpointChoice::Primary,
                        provider_tool_call_id: "pc_1".into(),
                        wire_name: "system.status".into(),
                        canonical_operation: "system.status".into(),
                        arguments_json: "{}".into(),
                        reasoning_content: None,
                    }),
                ),
                1 => (
                    crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                        id: "tc_2".into(),
                        operation: "system.status".into(),
                        arguments: json!({}),
                    }),
                    Some(crate::llm::ProviderToolTurn {
                        endpoint: crate::llm::EndpointChoice::Primary,
                        provider_tool_call_id: "pc_2".into(),
                        wire_name: "system.status".into(),
                        canonical_operation: "system.status".into(),
                        arguments_json: "{}".into(),
                        reasoning_content: None,
                    }),
                ),
                _ => (crate::llm::ToolCallResult::Absent, None),
            };
            Ok(crate::llm::LlmOutput {
                provider: "rec".into(),
                model: "rec".into(),
                content: if round == 2 { "final answer".into() } else { String::new() },
                journal_payload: json!({}),
                tool_call,
                provider_turn,
            })
        }
    }

    /// The production LLM adapter must hand ALL accumulated tool results to
    /// the third model call: the bounded loop (2 tools) runs through the
    /// real `RuntimeV0Model`, and the third `LlmClient::complete` invocation
    /// must carry both prior results in order.
    #[test]
    fn runtime_v0_host_loop_passes_accumulated_results_to_third_llm_call() -> Result<()> {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let llm: Box<dyn LlmClient> = Box::new(RecordingLlm {
            seen: seen.clone(),
            rounds: std::cell::Cell::new(0),
        });
        let journal = JournalStore::in_memory().unwrap();
        let session = journal
            .get_or_create_session(&SessionTarget {
                agent_id: AgentId("main".into()),
                channel: ChannelKind::Cli,
                conversation_key: "ctx-test".into(),
            })
            .unwrap();
        let run = Run {
            id: crate::domain::RunId::new(),
            session_id: session.id.clone(),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: principal_with_id("cli:local"),
            parent_run_id: None,
            delegated_by: None,
            status: crate::domain::RunStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            registry_snapshot_id: String::new(),
            mode: crate::domain::RunMode::Default,
            budget_hook_id: None,
            budget_hook_version: None,
            budget_decision_digest: None,
            budget_max_tool_rounds: None,
            budget_max_wall_time_ms: None,
            budget_exhaustion_action: None,
        };
        let snapshot = journal
            .load_registry_snapshot(&journal.current_registry_snapshot_id().unwrap())
            .unwrap();
        let model = RuntimeV0Model::new(
            llm,
            vec![],
            snapshot,
            None,
            None,
            &journal,
            run,
            session,
        );
        struct OkPort;
        impl InvocationPort for OkPort {
            fn submit(
                &self,
                invocation_id: &str,
                _capability_ref: &str,
                _arguments: Value,
            ) -> Result<InvocationResult, String> {
                Ok(InvocationResult {
                    invocation_id: invocation_id.into(),
                    status: InvocationStatus::Succeeded,
                    output: json!({"stdout": "out"}),
                })
            }
        }
        let tool = agent_runtime::Tool {
            name: "system.status".into(),
            description: String::new(),
            parameters: json!({}),
        };
        let mut runtime = RuntimeLoop::with_max_tool_calls(OkPort, model, 3);
        let reply = runtime.run("go", &[tool]).expect("loop");
        assert_eq!(reply, "final answer", "reply must come from the third model call");

        let seen = seen.borrow();
        assert_eq!(seen.len(), 3, "exactly three LLM calls");
        assert!(seen[0].is_empty(), "first call sees nothing");
        assert_eq!(seen[1].len(), 1, "second call sees result A");
        assert_eq!(
            seen[2].len(),
            2,
            "third call MUST see both prior tool results: {:?}",
            *seen
        );
        Ok(())
    }

    /// A snapshot carrying an External context.prepare.v0 binding.
    fn snapshot_with_context_binding(endpoint: &str, provider: &str) -> crate::registry::snapshot::RegistrySnapshot {
        use crate::registry::snapshot::{BindingKind, HookBinding, RegistrySnapshot};
        let mut snapshot = RegistrySnapshot {
            snapshot_id: format!("snap_ctx_{}", endpoint.len()),
            created_at: Utc::now(),
            operations: vec![],
            hook_bindings: vec![],
        };
        snapshot.hook_bindings.push(HookBinding {
            contract: crate::registry::snapshot::CONTEXT_PREPARE_HOOK_CONTRACT.to_string(),
            hook_id: "external:context-harness-v0".into(),
            hook_version: "v0".into(),
            binding_kind: BindingKind::External,
            binding_key: "external.context_harness".into(),
            provider_id: provider.into(),
            endpoint: endpoint.into(),
        });
        snapshot
    }

    #[test]
    fn pinned_snapshot_binding_provides_provider_and_endpoint() {
        let snap_a = snapshot_with_context_binding("http://127.0.0.1:7401/context", "agent-context-harness-v0");
        let binding = context_hook_binding(&snap_a).expect("external binding present");
        assert_eq!(binding.endpoint, "http://127.0.0.1:7401/context");
        assert_eq!(binding.provider_id, "agent-context-harness-v0");
    }

    #[test]
    fn missing_binding_never_falls_back_to_env_provider() {
        // Snapshot without any context.prepare.v0 binding: no hook, and no
        // env provider is consulted (the helper returns None).
        let snap = crate::registry::snapshot::RegistrySnapshot {
            snapshot_id: "snap_no_binding".into(),
            created_at: Utc::now(),
            operations: vec![],
            hook_bindings: vec![crate::registry::snapshot::HookBinding::builtin_budget()],
        };
        assert!(context_hook_binding(&snap).is_none());
        // A Builtin context binding also yields None (no builtin provider).
        let mut snap_builtin = crate::registry::snapshot::RegistrySnapshot {
            snapshot_id: "snap_builtin".into(),
            created_at: Utc::now(),
            operations: vec![],
            hook_bindings: vec![],
        };
        snap_builtin.hook_bindings.push(crate::registry::snapshot::HookBinding {
            contract: crate::registry::snapshot::CONTEXT_PREPARE_HOOK_CONTRACT.to_string(),
            hook_id: "builtin:context-default-v0".into(),
            hook_version: "v0".into(),
            binding_kind: crate::registry::snapshot::BindingKind::Builtin,
            binding_key: "builtin.context_default".into(),
            provider_id: String::new(),
            endpoint: String::new(),
        });
        assert!(context_hook_binding(&snap_builtin).is_none());
    }

    #[test]
    fn credential_mismatch_fails_closed_and_env_mechanics_are_preserved() {
        let snap_a = snapshot_with_context_binding("http://127.0.0.1:7401/context", "agent-context-harness-v0");
        let binding = context_hook_binding(&snap_a).unwrap();

        // Local credential provider must match the binding.
        let wrong = HookConfig {
            provider_id: "other-provider".into(),
            ..Default::default()
        };
        assert!(validate_context_credential(binding, &wrong).is_err());

        // Env keeps the local call mechanics; provider/endpoint come from the
        // binding.
        let local = HookConfig {
            provider_id: "agent-context-harness-v0".into(),
            shared_secret: "local-secret".into(),
            timeout_ms: 1234,
            max_request_bytes: 2048,
            max_response_bytes: 4096,
            failure_mode: crate::hook::HookFailureMode::FailOpen,
            ..Default::default()
        };
        validate_context_credential(binding, &local).expect("matching credential");
        let config = build_context_hook_config(binding, &local);
        assert_eq!(config.endpoint.url, "http://127.0.0.1:7401/context");
        assert_eq!(config.provider_id, "agent-context-harness-v0");
        assert_eq!(config.shared_secret, "local-secret");
        assert_eq!(config.timeout_ms, 1234);
        assert_eq!(config.max_request_bytes, 2048);
        assert_eq!(config.max_response_bytes, 4096);
        assert_eq!(config.failure_mode, crate::hook::HookFailureMode::FailOpen);
        assert!(config.enabled);
    }
}
