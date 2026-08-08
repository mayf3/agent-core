//! FIRST production Canary: an engine switch that routes every message
//! through the standalone `agent-runtime` crate while it is ON.
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
//! Canary switch: `AGENT_CORE_RUNTIME_CANARY_ENABLED`. Off (default) →
//! everyone stays on the legacy Runtime. On → every message goes through
//! the new Runtime. NO identity selection: future multi-agent routing
//! belongs to an external Router Harness, not this switch. On failure this
//! path NEVER falls back to the legacy Runtime for the same message — the
//! Run is failed and the message is done.

use crate::config::KernelConfig;
use crate::domain::{
    JournalEventKind, RunId, RunPrincipal, RuntimeEventPayload, ValidatedEvent,
};
use crate::gateway::Gateway;
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

    let model = RuntimeV0Model::new(
        Box::new(super::delivery::build_llm_from_config(config)),
        provider_tools,
    );
    let port = RuntimeV0InvocationAdapter {
        runtime: &runtime,
        journal,
        gateway,
        run_id: run.id.clone(),
        turn_index: 0,
        tool_index: 0,
        remaining_ms: config.tool_loop_timeout_ms,
    };
    let mut loop_runtime = RuntimeLoop::new(port, model);
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

/// Thin adapter wrapping the production LLM as `agent_runtime::Model`.
///
/// Only the minimal conversions: turn -> LlmInput (no full ContextAssembler,
/// no history, no memory, no compaction) and LlmOutput -> ModelOutput. The
/// provider transcript needed for the second round is remembered here,
/// host-side.
struct RuntimeV0Model {
    llm: Box<dyn LlmClient>,
    tools_json: Vec<Value>,
    prior_provider_turn: Option<ProviderToolTurn>,
}

impl RuntimeV0Model {
    fn new(llm: Box<dyn LlmClient>, tools_json: Vec<Value>) -> Self {
        Self {
            llm,
            tools_json,
            prior_provider_turn: None,
        }
    }
}

impl Model for RuntimeV0Model {
    fn complete(&mut self, turn: &Turn) -> Result<ModelOutput, String> {
        let follow_ups: Vec<LlmFollowUp> = if turn.follow_ups.is_empty() {
            vec![]
        } else {
            // Second round: replay the first round's provider transcript so
            // the real tool result reaches the model as a proper tool turn.
            let provider_turn = self
                .prior_provider_turn
                .clone()
                .ok_or_else(|| "missing_provider_turn_for_follow_up".to_string())?;
            turn.follow_ups
                .iter()
                .map(|fr| LlmFollowUp {
                    provider_turn: provider_turn.clone(),
                    result_content: fr.text.clone(),
                })
                .collect()
        };
        let input = LlmInput {
            blocks: vec![], // minimal single-round context for the first Canary
            user_text: turn.user_text.clone(),
            granted_operations: vec![],
            provider_tools: self.tools_json.clone(),
            follow_ups,
            timeout_override_ms: None,
        };
        let output = self.llm.complete(input).map_err(|e| e.to_string())?;
        self.prior_provider_turn = output.provider_turn.clone();
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
    turn_index: usize,
    tool_index: usize,
    remaining_ms: u64,
}

impl InvocationPort for RuntimeV0InvocationAdapter<'_> {
    fn submit(
        &self,
        invocation_id: &str,
        capability_ref: &str,
        arguments: Value,
    ) -> Result<InvocationResult, String> {
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
                self.turn_index,
                self.tool_index,
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
    fn canary_enabled_delivers_through_new_runtime() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let mut config = KernelConfig::from_cli(None);
        config.runtime_canary_enabled = true;
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

    /// Switch off by default → every message keeps the legacy Runtime:
    /// the legacy ContextAssembler runs (ContextBuilt).
    #[test]
    fn canary_disabled_by_default_keeps_legacy_runtime() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let config = KernelConfig::from_cli(None); // runtime_canary_enabled defaults to false
        let gateway = Gateway::new(config.clone());

        super::super::delivery::deliver_event(config, &journal, &gateway, validated_cli_event("hello"))?;

        let events = journal.events()?;
        assert!(
            events.iter().any(|e| e.kind == JournalEventKind::ContextBuilt),
            "legacy path must run the ContextAssembler when the switch is off"
        );
        Ok(())
    }

    /// Switch explicitly off → legacy Runtime, exactly like the default.
    #[test]
    fn canary_disabled_explicitly_keeps_legacy_runtime() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let mut config = KernelConfig::from_cli(None);
        config.runtime_canary_enabled = false;
        let gateway = Gateway::new(config.clone());

        super::super::delivery::deliver_event(config, &journal, &gateway, validated_cli_event("hello"))?;

        let events = journal.events()?;
        assert!(
            events.iter().any(|e| e.kind == JournalEventKind::ContextBuilt),
            "legacy path must run the ContextAssembler when explicitly disabled"
        );
        Ok(())
    }

    /// Failure on the new path must NOT fall back to the legacy Runtime:
    /// with the switch ON, a principal without channel grants reaches the
    /// reply approval, which fails deterministically. The Run is marked
    /// Failed, the delivery errors, and no second Run is ever created — the
    /// legacy Runtime is never invoked for the same message.
    #[test]
    fn canary_failure_never_falls_back_to_legacy_runtime() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let mut config = KernelConfig::from_cli(None);
        config.runtime_canary_enabled = true;
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
}
