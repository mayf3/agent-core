use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCallResult};
use crate::runtime::tool_rejection::sanitize_operation_for_audit;
use anyhow::Result;
use serde_json::json;

pub(crate) const MAX_TOOL_ROUNDS: usize = 2;

/// Outcome of an inline tool-call attempt. The text is the model-visible
/// ToolResult content; `Fatal` indicates the tool loop must abort (an
/// infrastructure failure that cannot be fed back to the model).
pub(crate) enum ToolCallOutcome {
    /// A ToolResult was produced (success, business rejection, or execution
    /// failure) and the loop may continue with another LLM round. The text
    /// distinguishes the three outcomes the model can act on: `rejected`,
    /// `execution_failed`, and `succeeded` (the tool's own output).
    ToolResult { text: String },
    /// An infrastructure failure that cannot be recovered: the Run must be
    /// terminated with the accurate run_id.
    Fatal { category: &'static str },
}

impl<L: LlmClient> super::Runtime<L> {
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
        // Monotonic tool counter across the whole loop: distinct tool calls
        // (malformed or valid) get distinct indices, so idempotency keys and
        // internal ids never collide even if the provider reuses a tool_call.id.
        let mut tool_index: usize = 0;
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
                                content: text,
                                compressibility: Compressibility::Summarizable,
                                source_ref: Some("tool:malformed".to_string()),
                            });
                            let next = self.complete_after_tool_result(
                                journal, run, session, blocks, user_text,
                            )?;
                            llm = next;
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
                            blocks.push(ContextBlock {
                                kind: ContextBlockKind::ToolResult,
                                content: format!("tool: {op_for_ref}\nresult: {text}"),
                                compressibility: Compressibility::Summarizable,
                                source_ref: Some(format!("tool:{op_for_ref}")),
                            });
                            let next = self.complete_after_tool_result(
                                journal, run, session, blocks, user_text,
                            )?;
                            llm = next;
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
    ) -> Result<LlmOutput> {
        let next = match self.llm.complete(LlmInput {
            blocks: blocks.to_vec(),
            user_text: user_text.to_string(),
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

    /// Terminate a Run with the accurate run_id after an infrastructure failure
    /// that cannot be fed back to the model. Writes a `RunFailed` fact (with
    /// the run_id — never `None`) and sets `runs.status` to `Failed`. Returns
    /// the propagated infrastructure error so `deliver()` surfaces it.
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
    ) -> Result<(crate::domain::ReceiptStatus, serde_json::Value, String)> {
        crate::runtime::tool_rejection::execute_session_recall(journal, session_id, approved)
    }
}

#[cfg(test)]
#[path = "tool_loop_tests.rs"]
mod tool_loop_tests;

#[cfg(test)]
#[path = "blank_reply_tests.rs"]
mod blank_reply_tests;
