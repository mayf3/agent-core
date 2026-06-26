use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmFollowUp, LlmInput, LlmOutput, ProviderToolTurn, ToolCallResult};
use crate::runtime::tool_rejection::sanitize_operation_for_audit;
use anyhow::Result;
use serde_json::json;

pub(crate) const MAX_TOOL_ROUNDS: usize = 2;

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
    ) -> Result<LlmOutput> {
        let mut tool_index: usize = 0;
        // Run-local follow-up state: the provider turn from the first round,
        // carried explicitly through LlmInput — never shared client state.
        let mut pending_turn: Option<ProviderToolTurn> = llm.provider_turn.take();
        for turn_index in 0..MAX_TOOL_ROUNDS {
            match llm.tool_call.clone() {
                ToolCallResult::Absent => return Ok(llm),
                ToolCallResult::Malformed(_reason) => {
                    let this_tool = tool_index;
                    tool_index += 1;
                    let outcome = self
                        .handle_malformed_tool_call(journal, run, session, turn_index, this_tool)?;
                    match outcome {
                        ToolCallOutcome::Fatal { category } => {
                            return self.terminate_run_failure(journal, run, session, category);
                        }
                        ToolCallOutcome::ToolResult { text } => {
                            blocks.push(ContextBlock {
                                kind: ContextBlockKind::ToolResult,
                                content: text.clone(),
                                compressibility: Compressibility::Summarizable,
                                source_ref: Some("tool:malformed".to_string()),
                            });
                            let fu = pending_turn.take().map(|pt| LlmFollowUp {
                                provider_turn: pt,
                                result_content: text,
                            });
                            llm = self.complete_after_tool_result(
                                journal, run, session, blocks, user_text, fu,
                            )?;
                            if llm.tool_call.is_absent() {
                                return Ok(llm);
                            }
                            continue;
                        }
                    }
                }
                ToolCallResult::Valid(tool_call) => {
                    let this_tool = tool_index;
                    tool_index += 1;
                    let outcome = self.handle_inline_tool_call(
                        journal, gateway, run, session, &tool_call, turn_index, this_tool,
                    )?;
                    match outcome {
                        ToolCallOutcome::Fatal { category } => {
                            return self.terminate_run_failure(journal, run, session, category);
                        }
                        ToolCallOutcome::ToolResult { text } => {
                            let op_for_ref = sanitize_operation_for_audit(&tool_call.operation);
                            // The structured ToolResult block is the only
                            // ToolResult in the system context (do NOT also send
                            // it as a role:tool message — that would duplicate).
                            blocks.push(ContextBlock {
                                kind: ContextBlockKind::ToolResult,
                                content: format!("tool: {op_for_ref}\nresult: {text}"),
                                compressibility: Compressibility::Summarizable,
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
                            llm = self.complete_after_tool_result(
                                journal, run, session, blocks, user_text, fu,
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
            llm.content = format!(
                "{}\n\n[Reached tool-call limit ({MAX_TOOL_ROUNDS}). Using the best answer from the last round.]",
                if llm.content.trim().is_empty() {
                    "I gathered information but couldn't finish within the tool-call limit."
                } else {
                    &llm.content
                }
            );
        }
        Ok(llm)
    }

    fn complete_after_tool_result(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        blocks: &[ContextBlock],
        user_text: &str,
        follow_up: Option<LlmFollowUp>,
    ) -> Result<LlmOutput> {
        let next = match self.llm.complete(LlmInput {
            blocks: blocks.to_vec(),
            user_text: user_text.to_string(),
            granted_operations: run
                .principal
                .grants
                .iter()
                .map(|g| g.operation.clone())
                .collect(),
            follow_up,
        }) {
            Ok(next) => next,
            Err(_) => {
                return self.terminate_run_failure(
                    journal,
                    run,
                    session,
                    "tool_followup_llm_failed",
                )
            }
        };
        if journal
            .append_event(
                JournalEventKind::LlmCompleted,
                Some(&run.id),
                Some(&session.id),
                None,
                next.journal_payload.clone(),
            )
            .is_err()
        {
            return self.terminate_run_failure(
                journal,
                run,
                session,
                "tool_followup_journal_failed",
            );
        }
        Ok(next)
    }

    fn terminate_run_failure(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        category: &'static str,
    ) -> Result<LlmOutput> {
        let run_status_recorded = journal.fail_run(&run.id).is_ok();
        let failure_fact_recorded = journal
            .append_event(
                JournalEventKind::RunFailed,
                Some(&run.id),
                Some(&session.id),
                None,
                json!({ "run_id": run.id.0, "error_category": category }),
            )
            .is_ok();
        Err(anyhow::anyhow!(
            "tool loop infrastructure failure: {category}; run_status_recorded={run_status_recorded}; failure_fact_recorded={failure_fact_recorded}"
        ))
    }

    pub(crate) fn execute_session_recall(
        journal: &JournalStore,
        session_id: &SessionId,
        approved: &ApprovedInvocation,
    ) -> Result<(ReceiptStatus, serde_json::Value, String)> {
        crate::runtime::tool_rejection::execute_session_recall(journal, session_id, approved)
    }
}

#[cfg(test)]
#[path = "tool_loop_tests.rs"]
mod tool_loop_tests;

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
