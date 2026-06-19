use crate::adapters::InvocationAdapter;
use crate::config::KernelConfig;
use crate::context::ContextAssembler;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput};
use crate::server::DispatcherMetrics;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::json;

pub mod outbox_dispatcher;

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
    L: LlmClient,
{
    pub fn new(config: KernelConfig, llm: L) -> Self {
        Self { config, llm }
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
    /// Phase 2 tool-call MVP: if the model emitted a `ReadOnly` tool call,
    /// validate it, execute `TimeAdapter` inline, and journal the receipt —
    /// WITHOUT queueing into `outbox_dispatches` (the outbox dispatcher is
    /// wired to `HttpConnectorAdapter`; a local adapter must not route there).
    /// Audit facts: `InvocationProposed` + `InvocationApproved` +
    /// `ReceiptReceived`. `OutboxQueued`/`DispatchStarted` are intentionally
    /// skipped (see docs/decisions/tool-call-execution-loop.md §4.5).
    /// Returns the receipt output serialized as text for the run outcome, or
    /// None if no tool call was emitted / it was rejected.
    fn handle_inline_tool_call(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        tool_call: &crate::llm::ToolCall,
    ) -> Result<Option<String>> {
        let mut intent = match crate::gateway::validate_tool_call(tool_call, &run.id) {
            Ok(intent) => intent,
            Err(e) => {
                // Rejection is surfaced as a ToolResult-style note, not a crash.
                return Ok(Some(format!("tool call rejected: {}", e)));
            }
        };
        if let Some(arguments) = intent.arguments.as_object_mut() {
            // The model may ask for a tool, but it must not choose the target
            // session. The Runtime pins tool-call intents to the current run's
            // session before the policy pipeline runs.
            arguments.insert("session_id".to_string(), json!(session.id.0));
        }
        let correlation_id = intent.invocation_id.0.clone();
        journal.append_event(
            JournalEventKind::InvocationProposed,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "operation": intent.operation,
                "idempotency_key": intent.idempotency_key,
                "source": "model_tool_call",
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
        // Route to the correct inline executor by operation name.
        let (receipt_status, receipt_output, result_text) =
            match approved.intent().operation.as_str() {
                crate::domain::operation::TIME_NOW => {
                    let receipt = crate::adapters::TimeAdapter.execute(&approved)?;
                    let text = receipt
                        .output
                        .get("iso")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ok")
                        .to_string();
                    (receipt.status, receipt.output, text)
                }
                crate::domain::operation::SESSION_RECALL_RECENT => {
                    let (status, output, text) =
                        Self::execute_session_recall(journal, &session.id, &approved);
                    (status, output, text)
                }
                other => {
                    // A read-only operation not yet wired for inline execution.
                    (
                        crate::domain::ReceiptStatus::Failed,
                        json!({ "error": format!("inline execution not implemented for {other}") }),
                        format!("tool not implemented: {other}"),
                    )
                }
            };
        journal.append_event(
            JournalEventKind::ReceiptReceived,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "invocation_id": approved.intent().invocation_id,
                "status": format!("{:?}", receipt_status),
                "output": receipt_output,
            }),
        )?;
        Ok(Some(result_text))
    }

    /// Execute `session.recall_recent`: read recent user messages from the
    /// **current session only** and return a normalized, sanitized result.
    ///
    /// Security invariants:
    /// - Only reads the current session (`session_id` is pinned by the caller).
    /// - Never returns raw `payload_json`; only normalized `event_id` + `role`
    ///   + `text` (truncated to `MAX_RECALL_CHARS` per message).
    /// - `limit` is clamped to `1..=MAX_RECALL_LIMIT` (default 5).
    /// - Optional `query` filters by case-insensitive substring on text.
    /// - No cross-session access, no file system, no network.
    fn execute_session_recall(
        journal: &JournalStore,
        session_id: &SessionId,
        approved: &ApprovedInvocation,
    ) -> (crate::domain::ReceiptStatus, serde_json::Value, String) {
        const MAX_RECALL_LIMIT: usize = 20;
        const MAX_RECALL_CHARS: usize = 500;

        let args = &approved.intent().arguments;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, MAX_RECALL_LIMIT as u64) as usize)
            .unwrap_or(5);
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        let messages = match journal.recent_user_messages(session_id, limit) {
            Ok(msgs) => msgs,
            Err(_) => {
                return (
                    crate::domain::ReceiptStatus::Failed,
                    json!({ "error": "failed to read session history" }),
                    "session recall failed".to_string(),
                );
            }
        };

        let mut results: Vec<serde_json::Value> = Vec::new();
        for (event_id, text) in &messages {
            // Optional case-insensitive substring filter.
            if let Some(ref q) = query {
                if !text.to_lowercase().contains(q) {
                    continue;
                }
            }
            // Truncate per-message text to the safety limit.
            let truncated: String = text.chars().take(MAX_RECALL_CHARS).collect();
            results.push(json!({
                "event_id": event_id,
                "role": "user",
                "text": truncated,
            }));
        }

        let output = json!({
            "session_id": session_id.0,
            "count": results.len(),
            "messages": results,
        });

        // A compact text summary for the run outcome / ToolResult context.
        let text = if results.is_empty() {
            "no matching messages found".to_string()
        } else {
            results
                .iter()
                .filter_map(|m| m.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" | ")
        };

        (crate::domain::ReceiptStatus::Succeeded, output, text)
    }

    pub fn deliver(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        event: ValidatedEvent,
        dispatcher_metrics: Option<&DispatcherMetrics>,
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

        // Dogfood Loop 1: status keyword → bypass LLM, return journal summary.
        if let Some(status_text) = Self::maybe_status_reply(&text, journal, dispatcher_metrics)? {
            journal.append_event(JournalEventKind::ContextBuilt, Some(&run.id), Some(&session.id), None,
                json!({"block_count": 0, "status_shortcut": true}))?;
            journal.append_event(JournalEventKind::LlmCompleted, Some(&run.id), Some(&session.id), None,
                json!({"provider": "status-shortcut", "status": "ok", "context_blocks": 0}))?;
            let intent = self.reply_intent(&run, &session, &status_text, message_id, chat_id);
            let correlation_id = intent.invocation_id.0.clone();
            journal.append_event(JournalEventKind::InvocationProposed, Some(&run.id), Some(&session.id), Some(&correlation_id),
                json!({"operation": intent.operation, "idempotency_key": intent.idempotency_key, "source": "status_shortcut"}))?;
            let approved = gateway.approve_invocation(intent, &run, &session)?;
            journal.append_event(JournalEventKind::InvocationApproved, Some(&run.id), Some(&session.id), Some(&correlation_id),
                json!({"decision_id": approved.decision_id, "operation": approved.intent().operation}))?;
            self.enqueue_or_pause(journal, &approved, &run, &session, &correlation_id)?;
            return Ok(RuntimeOutcome { run_id: run.id, session_id: session.id, output: status_text });
        }

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

        // Phase 2 tool-call MVP: if the model emitted a ReadOnly tool call,
        // execute it inline (TimeAdapter) and surface the result. This does
        // not replace the reply path below — a model may emit both a text
        // reply and a tool call.
        if let Some(tc) = llm.tool_call.as_ref() {
            let _ = self.handle_inline_tool_call(journal, gateway, &run, &session, tc)?;
        }

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

    /// Dogfood Loop 1: detect a status-query keyword and return a deterministic
    /// health summary from the Journal, bypassing the LLM. Only aggregate counts
    /// — never secrets, payloads, or raw journal content.
    fn maybe_status_reply(
        text: &str,
        journal: &JournalStore,
        dispatcher_metrics: Option<&DispatcherMetrics>,
    ) -> Result<Option<String>> {
        let lower = text.trim().to_lowercase();
        let is_status_query = [
            "状态怎么样", "现在状态", "系统状态", "健康状况", "健康状态",
            "status", "health", "how are you", "how's it going",
        ]
        .iter()
        .any(|kw| lower.contains(kw));
        if !is_status_query {
            return Ok(None);
        }
        let hash_ok = journal.verify_hash_chain()?;
        let pending = journal.outbox_status_count(crate::domain::OutboxDispatchStatus::Pending)?;
        let unknown = journal.outbox_unknown_unacked_count()?;
        let dispatching = journal.outbox_status_count(crate::domain::OutboxDispatchStatus::Dispatching)?;
        let drift = journal.outbox_projection_drift_count()?;
        let undelivered = journal.undelivered_ingress_events()?.len() as i64;
        let awaiting_approval = journal.awaiting_approval_count()?;
        let event_count = journal.event_count()?;
        let stale_dispatching = journal.outbox_stale_dispatching_count()?;
        let rollup = if !hash_ok { "corrupt" } else if unknown > 0 || drift > 0 || undelivered > 0 { "degraded" } else { "ok" };
        let dispatcher_running = dispatcher_metrics.map(|m: &DispatcherMetrics| m.is_running());
        let last_tick = dispatcher_metrics.and_then(|m: &DispatcherMetrics| m.last_tick_at());
        let last_error = dispatcher_metrics.and_then(|m: &DispatcherMetrics| m.last_error_category());
        let mut reply = format!(
            "📊 Agent Core 状态\n\
             Rollup: {rollup}   Hash: {hash_status}\n\
             Events: {event_count}   Outbox dispatcher: {disp_status}\n\
             Outbox — pending: {pending}  dispatching: {dispatching}\n\
             unknown: {unknown}  stale: {stale_dispatching}   drift: {drift}\n\
             Ingress undelivered: {undelivered}\n\
             Approval awaiting: {awaiting_approval}",
            rollup = rollup,
            hash_status = if hash_ok { "✅" } else { "❌" },
            event_count = event_count,
            disp_status = match dispatcher_running { Some(true) => "✅", Some(false) => "❌", None => "N/A" },
            pending = pending, dispatching = dispatching, unknown = unknown,
            stale_dispatching = stale_dispatching, drift = drift,
            undelivered = undelivered, awaiting_approval = awaiting_approval,
        );
        if let Some(lt) = &last_tick { reply.push_str(&format!("\nLast dispatch tick: {lt}")); }
        if let Some(le) = &last_error { reply.push_str(&format!("\nLast error: {le}")); }
        Ok(Some(reply))
    }
}
