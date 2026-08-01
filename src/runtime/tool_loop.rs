use crate::domain::*;
use crate::gateway::Gateway;
use crate::hook::ExhaustionAction;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmFollowUp, LlmInput, LlmOutput, ProviderToolTurn, ToolCallResult};
use crate::registry::snapshot::RegistrySnapshot;
use crate::runtime::tool_rejection::sanitize_operation_for_audit;
use anyhow::Result;
use serde_json::json;
use std::time::Instant;

/// Static user-facing message when the LLM fails during processing.
/// NEVER includes internal error categories, stack traces, or provider details.
/// The Run is Failed but the user still gets a notification.
pub(crate) const FOLLOWUP_LLM_FAILED_MSG: &str =
    "这次处理在调用模型生成后续回复时失败了。工具执行结果已记录，但任务可能尚未完成。你可以发送「继续」让我接着处理。";

pub(crate) const INITIAL_LLM_FAILED_MSG: &str =
    "这次处理模型暂时不可用，任务尚未开始完成。请稍后重试。";

/// User-facing message when the budget is exhausted with `terminate` action.
/// The Run is marked Failed and the user is NOT told to send "继续".
const BUDGET_TERMINATED_MSG: &str = "本轮因预算耗尽而终止，任务尚未完成。如需继续，请重新发起。";

/// Single tool-call MVP: only `tool_calls[0]` is parsed and executed per round.

pub(crate) enum ToolCallOutcome {
    ToolResult { text: String },
    Fatal { category: &'static str },
}

impl<L: LlmClient + 'static> super::Runtime<L> {
    pub(crate) fn run_tool_recall_loop(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        blocks: &mut Vec<ContextBlock>,
        user_text: &str,
        mut llm: LlmOutput,
        snapshot: &RegistrySnapshot,
    ) -> Result<LlmOutput> {
        // Read the frozen budget from the Run. If unset (legacy Runs or
        // code paths that bypassed resolve_run_budget), fall back to the
        // config values so behaviour is identical to pre-V0.
        let max_rounds = run
            .budget_max_tool_rounds
            .map(|v| v as usize)
            .unwrap_or(self.config.max_tool_rounds);
        let timeout_ms = run
            .budget_max_wall_time_ms
            .unwrap_or(self.config.tool_loop_timeout_ms);
        let exhaustion_action = run
            .budget_exhaustion_action
            .unwrap_or(ExhaustionAction::Yield);
        // Run wall-clock deadline (High 2): frozen at the first loop entry.
        // Every LLM and tool invocation before the deadline; in-flight calls
        // carry the remaining time as their effective timeout so the caller
        // stops waiting AT the deadline, never after a natural return.
        let start = Instant::now();
        let deadline = start + std::time::Duration::from_millis(timeout_ms);
        let mut tool_index: usize = 0;
        // Pre-compute provider tools from the pinned snapshot — same list
        // for all LLM rounds of this Run.
        let provider_tools = snapshot.provider_tools_for_grants(
            &run.principal
                .grants
                .iter()
                .map(|g| g.operation.clone())
                .collect::<Vec<_>>(),
        );
        // Run-local follow-up state: the provider turn from the first round,
        // carried explicitly through LlmInput — never shared client state.
        let mut pending_turn: Option<ProviderToolTurn> = llm.provider_turn.take();
        let mut follow_ups: Vec<LlmFollowUp> = vec![];
        // Duplicate tool-call detection: tracks the last Valid tool call's
        // (operation, canonicalized-arguments) pair. Reset on Malformed or
        // Absent so only consecutive Valid calls trigger the check.
        let mut prev_tool_key: Option<(String, String)> = None;
        for turn_index in 0..max_rounds {
            match llm.tool_call.clone() {
                ToolCallResult::Absent => return Ok(llm),
                ToolCallResult::Malformed(_reason) => {
                    prev_tool_key = None;
                    let this_tool = tool_index;
                    tool_index += 1;
                    let outcome = self
                        .handle_malformed_tool_call(journal, run, session, turn_index, this_tool)?;
                    match outcome {
                        ToolCallOutcome::Fatal { category } => {
                            return self.handle_fatal_failure(journal, run, session, category);
                        }
                        ToolCallOutcome::ToolResult { text } => {
                            blocks.push(ContextBlock {
                                kind: ContextBlockKind::ToolResult,
                                content: text.clone(),
                                source_ref: Some("tool:malformed".to_string()),
                            });
                            let fu = pending_turn.take().map(|pt| LlmFollowUp {
                                provider_turn: pt,
                                result_content: text,
                            });
                            if let Some(fu) = fu {
                                follow_ups.push(fu);
                            }
                            // Wall-clock timeout: stop before the next LLM call.
                            if Self::check_wall_clock_timeout(
                                deadline,
                                timeout_ms,
                                exhaustion_action,
                                journal,
                                run,
                                session,
                                &mut llm,
                            )? {
                                return Ok(llm);
                            }
                            llm = self.complete_after_tool_result(
                                journal,
                                run,
                                session,
                                blocks,
                                user_text,
                                &provider_tools,
                                &follow_ups,
                                deadline,
                                timeout_ms,
                                exhaustion_action,
                            )?;
                            pending_turn = llm.provider_turn.take();
                            if llm.tool_call.is_absent() {
                                return Ok(llm);
                            }
                            continue;
                        }
                    }
                }
                ToolCallResult::Valid(tool_call) => {
                    // ----- Duplicate tool-call detection -----
                    let canonicalized = canonicalize_args_json(&tool_call.arguments);
                    let is_dup = is_mutating_coding_op(&tool_call.operation)
                        && prev_tool_key
                            .as_ref()
                            .map(|(op, args)| op == &tool_call.operation && args == &canonicalized)
                            .unwrap_or(false);
                    if is_dup {
                        let _ = journal.append_event(
                            JournalEventKind::ToolLoopDetected,
                            Some(&run.id),
                            Some(&session.id),
                            None,
                            json!({
                                "run_id": run.id.0,
                                "operation": tool_call.operation,
                                "turn_index": turn_index,
                            }),
                        );
                        llm.content = format!(
                            "{}\n\n检测到重复工具调用（{}），已自动停止。请发送「继续」以在下一 Run 中接着处理。",
                            if llm.content.trim().is_empty() {
                                "检测到重复工具调用，已自动停止。"
                            } else {
                                &llm.content
                            },
                            tool_call.operation,
                        );
                        return Ok(llm);
                    }
                    prev_tool_key = Some((tool_call.operation.clone(), canonicalized));
                    // ----- End duplicate detection -----

                    // Run deadline guard (High 2): do not START a tool call
                    // once the frozen Run budget deadline has passed. The
                    // remaining time is also passed down so the harness
                    // transport stops waiting at the deadline.
                    if Self::check_wall_clock_timeout(
                        deadline,
                        timeout_ms,
                        exhaustion_action,
                        journal,
                        run,
                        session,
                        &mut llm,
                    )? {
                        return Ok(llm);
                    }
                    let remaining_ms = deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64;
                    let this_tool = tool_index;
                    tool_index += 1;
                    let outcome = self.handle_inline_tool_call(
                        journal,
                        gateway,
                        run,
                        session,
                        &tool_call,
                        turn_index,
                        this_tool,
                        snapshot,
                        remaining_ms,
                    )?;
                    match outcome {
                        ToolCallOutcome::Fatal { category } => {
                            return self.handle_fatal_failure(journal, run, session, category);
                        }
                        ToolCallOutcome::ToolResult { text } => {
                            let op_for_ref = sanitize_operation_for_audit(&tool_call.operation);
                            // The structured ToolResult block is the only
                            // ToolResult in the system context (do NOT also send
                            // it as a role:tool message — that would duplicate).
                            blocks.push(ContextBlock {
                                kind: ContextBlockKind::ToolResult,
                                content: format!("tool: {op_for_ref}\nresult: {text}"),
                                source_ref: Some(format!("tool:{op_for_ref}")),
                            });
                            // Build the Run-local follow-up from the provider
                            // turn captured in the first-round LlmOutput. The
                            // endpoint identity comes from the actual HTTP
                            // request site — never inferred from turn_index.
                            let fu = pending_turn.take().map(|pt| LlmFollowUp {
                                provider_turn: pt,
                                result_content: text.clone(),
                            });
                            if let Some(fu) = fu {
                                follow_ups.push(fu);
                            }
                            // Wall-clock timeout: stop before the next LLM call.
                            if Self::check_wall_clock_timeout(
                                deadline,
                                timeout_ms,
                                exhaustion_action,
                                journal,
                                run,
                                session,
                                &mut llm,
                            )? {
                                return Ok(llm);
                            }
                            llm = self.complete_after_tool_result(
                                journal,
                                run,
                                session,
                                blocks,
                                user_text,
                                &provider_tools,
                                &follow_ups,
                                deadline,
                                timeout_ms,
                                exhaustion_action,
                            )?;
                            pending_turn = llm.provider_turn.take();
                            if llm.tool_call.is_absent() {
                                return Ok(llm);
                            }
                        }
                    }
                }
            }
        }
        if !llm.tool_call.is_absent() {
            // Record the budget exhaustion fact.
            let _ = journal.append_event(
                JournalEventKind::ToolBudgetExhausted,
                Some(&run.id),
                Some(&session.id),
                None,
                json!({"run_id": run.id.0, "tool_rounds_used": tool_index, "max_tool_rounds": max_rounds, "exhaustion_action": match exhaustion_action {
                    ExhaustionAction::Terminate => "terminate",
                    ExhaustionAction::Yield => "yield",
                }}),
            );
            match exhaustion_action {
                ExhaustionAction::Terminate => {
                    // Mark the Run as Failed and emit an explicit terminal
                    // event. The user sees a clear "budget exhausted" message
                    // and is NOT told to send "继续".
                    let _ = journal.fail_run(&run.id);
                    let _ = journal.append_event(
                        JournalEventKind::RunBudgetTerminated,
                        Some(&run.id),
                        Some(&session.id),
                        None,
                        json!({"run_id": run.id.0, "reason": "rounds", "max_tool_rounds": max_rounds, "used": tool_index}),
                    );
                    llm.content = format!(
                        "{}\n\n{BUDGET_TERMINATED_MSG}",
                        if llm.content.trim().is_empty() {
                            "本轮已达到工具执行上限。"
                        } else {
                            &llm.content
                        },
                    );
                }
                ExhaustionAction::Yield => {
                    // High 3: yield produces NO user-facing "请发送继续" text.
                    // The structured yield fact (ToolBudgetExhausted with
                    // exhaustion_action=yield) is the only artifact; the
                    // external Agent Loop Harness observes it and decides
                    // whether to continue. The model's own content is kept
                    // untouched — nothing here is ever delivered as a reply.
                }
            }
        }
        Ok(llm)
    }

    /// Check whether the Run wall-clock deadline has been reached. Called
    /// before every LLM and tool invocation. Returns `true` if the deadline
    /// passed (event already written, llm.content updated with the frozen
    /// yield/terminate semantics).
    fn check_wall_clock_timeout(
        deadline: Instant,
        timeout_ms: u64,
        exhaustion_action: ExhaustionAction,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        llm: &mut LlmOutput,
    ) -> Result<bool> {
        let elapsed_ms = timeout_ms.saturating_sub(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
        );
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            return Ok(false);
        }
        let _ = journal.append_event(
            JournalEventKind::ToolLoopWallClockExceeded,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({"run_id": run.id.0, "elapsed_ms": elapsed_ms, "timeout_ms": timeout_ms, "exhaustion_action": match exhaustion_action {
                ExhaustionAction::Terminate => "terminate",
                ExhaustionAction::Yield => "yield",
            }}),
        );
        match exhaustion_action {
            ExhaustionAction::Terminate => {
                let _ = journal.fail_run(&run.id);
                let _ = journal.append_event(
                    JournalEventKind::RunBudgetTerminated,
                    Some(&run.id),
                    Some(&session.id),
                    None,
                    json!({"run_id": run.id.0, "reason": "wall_clock", "max_wall_time_ms": timeout_ms, "elapsed_ms": elapsed_ms}),
                );
                llm.content = format!(
                    "{}\n\n{BUDGET_TERMINATED_MSG}",
                    if llm.content.trim().is_empty() {
                        "本轮已超过工具执行时间限制。"
                    } else {
                        &llm.content
                    },
                );
            }
            ExhaustionAction::Yield => {
                // High 3: yield produces NO user-facing "请发送继续" text. The
                // structured yield fact (ToolLoopWallClockExceeded with
                // exhaustion_action=yield) is the only artifact; the external
                // Agent Loop Harness observes it and decides whether to
                // continue. The model's own content is kept untouched.
            }
        }
        Ok(true)
    }

    fn complete_after_tool_result(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        blocks: &[ContextBlock],
        user_text: &str,
        provider_tools: &[serde_json::Value],
        follow_ups: &[LlmFollowUp],
        deadline: Instant,
        timeout_ms: u64,
        exhaustion_action: ExhaustionAction,
    ) -> Result<LlmOutput> {
        let next = match self.complete_model_invocation(
            journal,
            run,
            session,
            follow_ups.len(),
            LlmInput {
                timeout_override_ms: None,
                blocks: blocks.to_vec(),
                user_text: user_text.to_string(),
                granted_operations: run
                    .principal
                    .grants
                    .iter()
                    .map(|g| g.operation.clone())
                    .collect(),
                provider_tools: provider_tools.to_vec(),
                follow_ups: follow_ups.to_vec(),
            },
            Some(deadline),
        ) {
            Ok(next) => next,
            Err(error) => {
                if error.to_string().contains("run_deadline_exceeded") {
                    // Deadline reached before the LLM call started — apply the
                    // frozen exhaustion semantics instead of a generic failure.
                    let mut llm = LlmOutput {
                        provider: "system".into(),
                        model: "system".into(),
                        content: String::new(),
                        journal_payload: json!({"s":"deadline"}),
                        tool_call: ToolCallResult::Absent,
                        provider_turn: None,
                    };
                    let timed_out = Self::check_wall_clock_timeout(
                        deadline,
                        timeout_ms,
                        exhaustion_action,
                        journal,
                        run,
                        session,
                        &mut llm,
                    )?;
                    debug_assert!(timed_out, "deadline guard must report timeout");
                    return Ok(llm);
                }
                return self.handle_followup_llm_failure(journal, run, session);
            }
        };
        Ok(next)
    }

    /// Handle a fatal tool-loop infrastructure failure: record RunFailed and
    /// return a static failure LlmOutput so deliver() can enqueue a reply.
    fn handle_fatal_failure(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        category: &'static str,
    ) -> Result<LlmOutput> {
        journal.fail_run(&run.id)?;
        journal.append_event(
            JournalEventKind::RunFailed,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({ "run_id": run.id.0, "error_category": category }),
        )?;
        Ok(LlmOutput {
            provider: "system".into(),
            model: "system".into(),
            content: FOLLOWUP_LLM_FAILED_MSG.to_string(),
            journal_payload: json!({"s":"failure_notification"}),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }

    /// Enqueue a reply for a failed run without changing Run status (stays
    /// Failed). Uses a stable idempotency key scoped to this run so at most
    /// one failure notification is enqueued.
    pub(super) fn reply_with_failure(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        snapshot: &RegistrySnapshot,
        run: &Run,
        session: &Session,
        message_id: Option<String>,
        chat_id: Option<String>,
        text: &str,
    ) -> std::result::Result<super::RuntimeOutcome, anyhow::Error> {
        let mut intent = self.reply_intent(run, session, text, message_id, chat_id);
        intent.idempotency_key = Some(format!("failure-reply:{}", run.id.0));
        let correlation_id = intent.invocation_id.0.clone();
        journal.append_event(
            crate::domain::JournalEventKind::InvocationProposed,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            serde_json::json!({
                "operation": intent.operation,
                "idempotency_key": intent.idempotency_key,
            }),
        )?;
        let approved = gateway.approve_invocation(intent, run, session, snapshot)?;
        journal.append_event(
            crate::domain::JournalEventKind::InvocationApproved,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            serde_json::json!({
                "decision_id": approved.decision_id,
                "operation": approved.intent().operation,
            }),
        )?;
        journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
        Ok(super::RuntimeOutcome {
            run_id: run.id.clone(),
            session_id: session.id.clone(),
            output: text.to_string(),
        })
    }

    /// Record RunFailed and return a static failure LlmOutput (no LLM call).
    /// The caller (deliver) is responsible for creating the reply outbox entry.
    fn handle_followup_llm_failure(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
    ) -> Result<LlmOutput> {
        journal.fail_run(&run.id)?;
        journal.append_event(
            JournalEventKind::RunFailed,
            Some(&run.id),
            Some(&session.id),
            None,
            json!({ "run_id": run.id.0, "error_category": "tool_followup_llm_failed" }),
        )?;
        Ok(LlmOutput {
            provider: "system".into(),
            model: "system".into(),
            content: FOLLOWUP_LLM_FAILED_MSG.to_string(),
            journal_payload: json!({"s":"failure_notification"}),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Free-standing helpers (no Runtime receiver)
// ---------------------------------------------------------------------------

/// Returns true when `operation` is in the coding-harness mutating set for
/// which duplicate detection applies. Polling operations like
/// `external.coding_task_status` are excluded.
fn is_mutating_coding_op(operation: &str) -> bool {
    matches!(
        operation,
        "external.coding_workspace_write"
            | "external.coding_workspace_exec"
            | "external.coding_task_submit"
            | "external.coding_capability_propose"
    )
}

/// Produce a deterministic, JSON-key-sorted string from a `serde_json::Value`.
/// Used to canonicalize tool-call arguments so that semantically identical
/// argument sets (differing only in key ordering) produce the same digest.
fn canonicalize_args_json(val: &serde_json::Value) -> String {
    fn sort_keys(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut new_map = serde_json::Map::new();
                for k in keys {
                    new_map.insert(k.clone(), sort_keys(&map[k]));
                }
                serde_json::Value::Object(new_map)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(sort_keys).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort_keys(val)).unwrap_or_default()
}

#[cfg(test)]
#[path = "tool_loop_tests.rs"]
mod tool_loop_tests;

#[cfg(test)]
#[path = "tool_loop_extra_tests.rs"]
mod tool_loop_extra_tests;

#[cfg(test)]
#[path = "tests/tool_schema_recovery_tests.rs"]
mod tool_schema_recovery_tests;

#[cfg(test)]
#[path = "blank_reply_tests.rs"]
mod blank_reply_tests;

#[cfg(test)]
#[path = "grant_schema_tests.rs"]
pub(crate) mod grant_schema_tests;

#[cfg(test)]
#[path = "grants_context_tests.rs"]
pub(crate) mod grants_context_tests;

#[cfg(test)]
#[path = "tool_name_mode_tests.rs"]
pub(crate) mod tool_name_mode_tests;

#[cfg(test)]
#[path = "config_wiring_tests.rs"]
pub(crate) mod config_wiring_tests;

#[cfg(test)]
#[path = "transcript_isolation_tests.rs"]
pub(crate) mod transcript_isolation_tests;
