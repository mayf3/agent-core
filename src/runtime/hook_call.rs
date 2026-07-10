//! context.prepare.v0 hook invocation logic and Runtime delivery helpers,
//! extracted from `mod.rs` to keep that file under the 500-line limit.

use crate::domain::*;
use crate::gateway::Gateway;
use crate::hook::{
    ContextPrepareRequest, HookClient, HookConfig, HookFailureMode, HookKind, HookLimits,
};
use crate::journal::JournalStore;
use crate::registry::snapshot::RegistrySnapshot;
use crate::runtime::RuntimeOutcome;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::json;

/// Stub: session spawn is not yet enabled.
pub fn session_spawn() -> Result<()> {
    bail!("not_enabled:session.spawn")
}

/// Stub: run yield is not yet enabled.
pub fn run_yield() -> Result<()> {
    bail!("not_enabled:run.yield")
}

// ═══════════════════════════════════════════════════════════════════════════
// context.prepare.v0 hook logic
// ═══════════════════════════════════════════════════════════════════════════

pub(crate) enum HookCallOutcome {
    Injected,
    FailClosed { error: String },
    Skipped,
}

/// Invoke context.prepare.v0 and inject fragments into the blocks vector.
pub(crate) fn call_context_prepare(
    blocks: &mut Vec<ContextBlock>,
    hook_client: &dyn HookClient,
    hook_cfg: &HookConfig,
    journal: &JournalStore,
    run_id: &RunId,
    session_id: &SessionId,
    agent_id: &str,
    principal_id: &str,
    channel: &str,
    user_text: &str,
    context_budget_chars: usize,
) -> Result<HookCallOutcome> {
    let start = std::time::Instant::now();
    let prepare_req = ContextPrepareRequest {
        hook: HookKind::ContextPrepareV0,
        run_id: run_id.0.clone(),
        session_id: session_id.0.clone(),
        agent_id: agent_id.to_string(),
        principal: principal_id.to_string(),
        channel: channel.to_string(),
        user_text: user_text.to_string(),
        context_budget_chars,
    };
    let hook_result = hook_client.call_context_prepare(&prepare_req, hook_cfg);
    let duration_ms = start.elapsed().as_millis() as u64;
    match hook_result {
        Ok(resp) => {
            let limits: HookLimits = hook_cfg.into();
            let fragment_count = resp.fragments.len().min(hook_cfg.max_fragments);
            let resource_ref_count = resp.resource_refs.len();
            if let Some(pos) = blocks
                .iter()
                .position(|b| b.kind == ContextBlockKind::UserMessage)
            {
                for frag in resp.fragments.iter().take(hook_cfg.max_fragments) {
                    if frag.validate_against(&limits).is_ok() {
                        let content = format!("[hook:{}] {}", frag.hook_id, frag.content);
                        blocks.insert(
                            pos,
                            ContextBlock {
                                kind: ContextBlockKind::HookFragment,
                                content,
                                compressibility: Compressibility::DropWhole,
                                source_ref: Some(frag.source.clone()),
                            },
                        );
                    }
                }
            }
            journal.append_event(
                JournalEventKind::HookCallRecorded,
                Some(run_id),
                Some(session_id),
                None,
                json!({
                    "hook": "context.prepare.v0", "status": "ok",
                    "failure_mode": format!("{:?}", hook_cfg.failure_mode),
                    "fragment_count": fragment_count, "resource_ref_count": resource_ref_count,
                    "response_bytes": 0, "duration_ms": duration_ms,
                }),
            )?;
            Ok(HookCallOutcome::Injected)
        }
        Err(e) => {
            let error_msg = e.to_string();
            let (status, fmode) = match hook_cfg.failure_mode {
                HookFailureMode::FailClosed => ("failed", "fail_closed"),
                HookFailureMode::FailOpen => ("skipped", "fail_open"),
                HookFailureMode::Degrade => ("degraded", "degrade"),
                HookFailureMode::Disabled => ("disabled", "disabled"),
            };
            journal.append_event(
                JournalEventKind::HookCallRecorded,
                Some(run_id),
                Some(session_id),
                None,
                json!({
                    "hook": "context.prepare.v0", "status": status,
                    "failure_mode": fmode, "error_code": error_msg,
                    "fragment_count": 0, "resource_ref_count": 0,
                    "response_bytes": 0, "duration_ms": duration_ms,
                }),
            )?;
            match hook_cfg.failure_mode {
                HookFailureMode::FailClosed => Ok(HookCallOutcome::FailClosed { error: error_msg }),
                HookFailureMode::FailOpen
                | HookFailureMode::Degrade
                | HookFailureMode::Disabled => Ok(HookCallOutcome::Skipped),
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Runtime delivery helpers (echo, create_run, reply_intent)
// ═══════════════════════════════════════════════════════════════════════════

impl<L: crate::llm::LlmClient + 'static> super::Runtime<L> {
    /// Handle a "deliver_echo" request.
    pub fn deliver_echo(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        event: ValidatedEvent,
    ) -> Result<RuntimeOutcome> {
        let session = journal.get_or_create_session(&event.session_target)?;
        journal.append_event(JournalEventKind::SessionReady, None, Some(&session.id), Some(&event.event_id.0), json!({
            "session_id": session.id.0, "agent_id": session.agent_id.0,
            "channel": format!("{:?}", session.channel), "conversation_key": session.conversation_key,
        }))?;
        let snapshot_id = journal
            .current_registry_snapshot_id()
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        if snapshot_id.is_empty() {
            anyhow::bail!("registry_snapshot_invalid: snapshot ID is empty");
        }
        let snapshot = journal
            .load_registry_snapshot(&snapshot_id)
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        let run = self.create_run(journal, &session, &event, &snapshot_id, &snapshot);
        journal.insert_run(&run)?;
        journal.append_event(JournalEventKind::RunStarted, Some(&run.id), Some(&session.id),
            Some(&event.event_id.0), json!({"run_id": run.id.0, "trigger_event_id": run.trigger_event_id.0, "principal_id": run.principal.principal_id.0}))?;
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
                "operation": intent.operation, "idempotency_key": intent.idempotency_key,
            }),
        )?;
        let approved = gateway.approve_invocation(intent, &run, &session, &snap_for_gateway)?;
        journal.append_event(
            JournalEventKind::InvocationApproved,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "decision_id": approved.decision_id, "operation": approved.intent().operation,
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

    fn is_coding_owner(&self, principal: &RunPrincipal, chat_type: Option<&str>) -> bool {
        super::coding_grants::is_coding_owner(&self.config, principal, chat_type)
    }

    pub(crate) fn create_run(
        &self,
        journal: &JournalStore,
        session: &Session,
        event: &ValidatedEvent,
        snapshot_id: &str,
        snapshot: &RegistrySnapshot,
    ) -> Run {
        let now = Utc::now();
        let mut principal = event.principal.clone();
        let is_owner = self.is_coding_owner(&principal, event.chat_type.as_deref());
        super::coding_grants::augment_grants(&mut principal, snapshot, is_owner);

        // Load explicit external operation grants from the journal.
        // These grants are persisted in external_operation_grants via
        // JournalStore::create_external_operation_grant and are separate
        // from channel-default grants and owner coding grants.
        //
        // conversation_kind is derived from event.chat_type and the session
        // channel to distinguish Feishu private/p2p from group chat.
        // Fail-closed: unrecognized combinations map to "" which matches
        // no grant (conversation_kind has CHECK constraint p2p/group/cli).
        let conversation_kind = match (&session.channel, event.chat_type.as_deref()) {
            (ChannelKind::Cli, _) => "cli",
            (ChannelKind::Feishu, Some("p2p")) => "p2p",
            (ChannelKind::Feishu, Some("group")) => "group",
            _ => "",
        };
        if let Ok(explicit_grants) = journal.load_active_external_operation_grants(
            &principal.principal_id.0,
            &format!("{:?}", session.channel),
            conversation_kind,
            "principal_channel",
            snapshot_id,
        ) {
            for g in explicit_grants {
                if !principal
                    .grants
                    .iter()
                    .any(|gr| gr.operation == g.operation)
                {
                    principal.grants.push(CapabilityGrant {
                        operation: g.operation,
                        scope: g.scope,
                    });
                }
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

    pub(crate) fn reply_intent(
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
                arguments: json!({"session_id": session.id.0, "message_id": message_id.unwrap_or_default(), "chat_id": chat_id.unwrap_or_default(), "text": text}),
                idempotency_key: Some(format!("feishu-reply:{}", run.id.0)),
            }
        } else {
            InvocationIntent {
                invocation_id: InvocationId(format!("reply:{}", run.id.0)),
                run_id: run.id.clone(),
                operation: crate::domain::operation::STDOUT_SEND_TEXT.to_string(),
                arguments: json!({"session_id": session.id.0, "text": text}),
                idempotency_key: Some(format!("stdout-reply:{}", run.id.0)),
            }
        }
    }
}

pub(crate) fn ensure_nonblank_reply(content: &str) -> String {
    if content.trim().is_empty() {
        "No reply was generated for this turn.".to_string()
    } else {
        content.to_string()
    }
}
