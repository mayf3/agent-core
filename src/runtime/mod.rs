use crate::adapters::InvocationAdapter;
use crate::config::KernelConfig;
use crate::context::ContextAssembler;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput};
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::json;

pub mod outbox_dispatcher;

pub struct Runtime<L, A> {
    config: KernelConfig,
    llm: L,
    #[allow(dead_code)]
    adapter: A,
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

impl<L, A> Runtime<L, A>
where
    L: LlmClient,
    A: InvocationAdapter,
{
    pub fn new(config: KernelConfig, llm: L, adapter: A) -> Self {
        Self {
            config,
            llm,
            adapter,
        }
    }

    /// Phase 2 M2d: decide whether an approved invocation is dispatched now or
    /// paused for human approval, and persist the outcome.
    ///
    /// - `Risk::ReadOnly`, or `Risk::Write` when the operator has **not** opted
    ///   in (`require_write_approval == false`): queue the dispatch and mark the
    ///   run `WaitingDispatch` (the pre-M2d behavior, byte-identical).
    /// - `Risk::Write` when the operator **has** opted in: do NOT queue. Append
    ///   an `ApprovalRequested` fact carrying an `intent_snapshot` (so an
    ///   approve can reconstruct and queue the dispatch without re-running the
    ///   LLM) and mark the run `AwaitingApproval`. The run resumes later via
    ///   `Gateway::approve_run` / `Gateway::deny_run`.
    fn enqueue_or_pause(
        &self,
        journal: &JournalStore,
        approved: &ApprovedInvocation,
        run: &Run,
        session: &Session,
        correlation_id: &str,
    ) -> Result<()> {
        let risk = crate::domain::operation::lookup(&approved.intent().operation)
            .map(|spec| spec.risk)
            .unwrap_or(crate::domain::operation::Risk::Write);
        let pause =
            self.config.require_write_approval && risk == crate::domain::operation::Risk::Write;
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
        let run = self.create_run(&session, &event);
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
        let blocks =
            ContextAssembler::from_config(&self.config).build(journal, &session, &event, &text)?;
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
        let llm = self.llm.complete(LlmInput {
            blocks,
            user_text: text,
        })?;
        journal.append_event(
            JournalEventKind::LlmCompleted,
            Some(&run.id),
            Some(&session.id),
            None,
            llm.journal_payload,
        )?;

        let intent = self.reply_intent(&run, &session, &llm.content, message_id, chat_id);
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
        let approved = gateway.approve_invocation(intent, &run, &session)?;
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
        self.enqueue_or_pause(journal, &approved, &run, &session, &correlation_id)?;
        Ok(RuntimeOutcome {
            run_id: run.id,
            session_id: session.id,
            output: llm.content,
        })
    }

    pub fn deliver_echo(
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
        let run = self.create_run(&session, &event);
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
        let approved = gateway.approve_invocation(intent, &run, &session)?;
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
        self.enqueue_or_pause(journal, &approved, &run, &session, &correlation_id)?;
        Ok(RuntimeOutcome {
            run_id: run.id,
            session_id: session.id,
            output: reply,
        })
    }

    fn create_run(&self, session: &Session, event: &ValidatedEvent) -> Run {
        let now = Utc::now();
        Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: self.config.agent_id.clone(),
            trigger_event_id: event.event_id.clone(),
            principal: event.principal.clone(),
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
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
