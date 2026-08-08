//! Runtime V0 — the minimal production Agent loop.
//!
//! A self-contained model-turn loop that lives NEXT TO the legacy V1
//! `Runtime` in agent-core-kernel without restructuring it. It knows only
//! three things:
//!
//! 1. a model that speaks in plain turns ([`Model`]),
//! 2. a narrow invocation port ([`InvocationPort`]),
//! 3. the tool view the host supplies for the current conversation.
//!
//! The loop shape is the one proven by the Runtime V0 canary:
//!
//! ```text
//! model round 1 -> action -> Invocation -> result -> model round 2 -> final reply
//! ```
//!
//! It intentionally knows nothing about the older world: no JournalStore,
//! no Gateway, no RegistrySnapshot, no grants, no policy, no approval, no
//! execution-harness, no channel/product specifics. All of those live on
//! the compatibility side — the host that wires this loop into the legacy
//! Kernel.
//!
//! LEGACY COMPATIBILITY DEBT: this loop's caller connects to the legacy
//! Kernel through the `run_id` umbilical cord; the `Runtime -> run_id ->
//! Kernel` shape is NOT the final V2 Kernel boundary.
//!
//! This crate must stay import-clean of `agent-core-kernel` internals. The
//! Kernel (or any host) wires it in only through the narrow
//! [`InvocationPort`]; it never sees Kernel types. This keeps the physical
//! crate boundary identical to the logical boundary the Runtime needs.

use serde_json::Value;

/// The narrow invocation port — the only way this loop touches the outside.
pub trait InvocationPort {
    fn submit(
        &self,
        invocation_id: &str,
        capability_ref: &str,
        arguments: Value,
    ) -> Result<InvocationResult, String>;
}

/// What the outside currently knows about one submitted call.
pub enum InvocationStatus {
    Succeeded,
    Failed,
    Unknown,
}

pub struct InvocationResult {
    pub invocation_id: String,
    pub status: InvocationStatus,
    pub output: Value,
}

/// A tool this loop offers to the model (the host's own view).
#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// The model decided to take one action through the port.
pub struct Action {
    pub tool: String,
    pub arguments: Value,
}

/// One completed action fed back to the model as plain text.
#[derive(Clone)]
pub struct ToolResult {
    pub tool: String,
    pub text: String,
}

/// Everything a model needs for one turn.
pub struct Turn {
    pub user_text: String,
    pub tool_view: Vec<Tool>,
    pub follow_ups: Vec<ToolResult>,
}

pub struct ModelOutput {
    pub text: String,
    pub action: Option<Action>,
}

/// A model that speaks in turns. A `Result` error means the model call
/// itself failed (LLM failure) — the loop must propagate it honestly
/// instead of fabricating an output.
pub trait Model {
    fn complete(&mut self, turn: &Turn) -> Result<ModelOutput, String>;
}

/// The V0 loop: model turn -> (optional) one action through the port ->
/// the result comes back as a follow-up for the next model turn — repeated
/// until the model answers directly, the model fails, the port fails, or
/// the bounded tool-call budget is exceeded.
///
/// The loop counts REAL tool calls, not model rounds: after the final
/// allowed tool executes, the model still gets one more turn to summarize
/// before the limit can trip.
pub struct RuntimeLoop<P: InvocationPort, M: Model> {
    port: P,
    model: M,
    max_tool_calls: usize,
}

impl<P: InvocationPort, M: Model> RuntimeLoop<P, M> {
    /// Default: at most ONE real tool call (the original two-round shape).
    pub fn new(port: P, model: M) -> Self {
        Self::with_max_tool_calls(port, model, 1)
    }

    /// `max_tool_calls` is the maximum number of REAL tool executions. The
    /// loop knows nothing else about budgets or deadlines.
    pub fn with_max_tool_calls(port: P, model: M, max_tool_calls: usize) -> Self {
        Self {
            port,
            model,
            max_tool_calls,
        }
    }

    pub fn run(&mut self, user_text: &str, tool_view: &[Tool]) -> Result<String, String> {
        // Accumulated tool results: every executed tool's outcome stays
        // visible to every later model turn.
        let mut follow_ups: Vec<ToolResult> = vec![];
        loop {
            let turn = Turn {
                user_text: user_text.to_string(),
                tool_view: tool_view.to_vec(),
                follow_ups: follow_ups.clone(),
            };
            // Model failure propagates; no fake output, no fallback.
            let out = self.model.complete(&turn)?;
            let Some(action) = out.action else {
                // The model answered directly — this is the final reply.
                return Ok(out.text);
            };
            // The model wants another tool. If the budget is exhausted,
            // fail explicitly — NEVER execute, NEVER fabricate a reply.
            if follow_ups.len() >= self.max_tool_calls {
                return Err(format!(
                    "tool_call_limit_reached: {} (max_tool_calls={})",
                    action.tool, self.max_tool_calls
                ));
            }
            // The loop generates a stable id for this call before submitting.
            let invocation_id = format!("inv_{}", uuid::Uuid::new_v4().simple());
            // Port failure propagates; a business-level Failed/Unknown
            // result is handed to the model as a plain result — never
            // silently retried.
            let result = self.port.submit(&invocation_id, &action.tool, action.arguments)?;
            follow_ups.push(ToolResult {
                tool: action.tool,
                text: render(&result),
            });
            // Continue asking the model with the accumulated results.
        }
    }
}

fn render(result: &InvocationResult) -> String {
    match result.status {
        InvocationStatus::Succeeded => format!("ok: {}", result.output),
        InvocationStatus::Failed => format!("failed: {}", result.output),
        InvocationStatus::Unknown => format!("unknown: {}", result.output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fake port that answers with a fixed output — the module's own
    /// unit test needs no outside world.
    struct FakePort;

    impl InvocationPort for FakePort {
        fn submit(
            &self,
            _invocation_id: &str,
            _capability_ref: &str,
            _arguments: Value,
        ) -> Result<InvocationResult, String> {
            Ok(InvocationResult {
                invocation_id: _invocation_id.into(),
                status: InvocationStatus::Succeeded,
                output: json!({"stdout": "hello from v0"}),
            })
        }
    }

    /// Round 0 takes one action; round 1 reads the follow-up and replies.
    struct TwoRoundModel {
        round: u32,
    }

    impl Model for TwoRoundModel {
        fn complete(&mut self, turn: &Turn) -> Result<ModelOutput, String> {
            if self.round == 0 {
                self.round += 1;
                Ok(ModelOutput {
                    text: String::new(),
                    action: Some(Action {
                        tool: "c17".into(),
                        arguments: json!({"command": "echo", "args": ["x"]}),
                    }),
                })
            } else {
                let seen = turn
                    .follow_ups
                    .first()
                    .map(|f| f.text.clone())
                    .unwrap_or_default();
                Ok(ModelOutput {
                    text: format!("final reply: {seen}"),
                    action: None,
                })
            }
        }
    }

    #[test]
    fn two_round_loop_submits_and_returns_final_reply() {
        let port = FakePort;
        let mut runtime = RuntimeLoop::new(port, TwoRoundModel { round: 0 });
        let tool = Tool {
            name: "c17".into(),
            description: "run a command".into(),
            parameters: json!({"type": "object"}),
        };
        let reply = runtime.run("hello", &[tool]).expect("loop");
        assert!(reply.starts_with("final reply:"), "got: {reply}");
        assert!(reply.contains("hello from v0"), "got: {reply}");
    }

    #[test]
    fn loop_returns_plain_reply_when_model_takes_no_action() {
        struct ReplyOnly;
        impl Model for ReplyOnly {
            fn complete(&mut self, _turn: &Turn) -> Result<ModelOutput, String> {
                Ok(ModelOutput { text: "no tools needed".into(), action: None })
            }
        }
        let port = FakePort;
        let mut runtime = RuntimeLoop::new(port, ReplyOnly);
        let reply = runtime.run("hello", &[]).expect("loop");
        assert_eq!(reply, "no tools needed");
    }

    /// A port that counts submissions — used to prove an Invocation is
    /// executed exactly once even when a later model call fails.
    struct CountingPort {
        submits: std::cell::Cell<u32>,
    }

    impl InvocationPort for CountingPort {
        fn submit(
            &self,
            _invocation_id: &str,
            _capability_ref: &str,
            _arguments: Value,
        ) -> Result<InvocationResult, String> {
            self.submits.set(self.submits.get() + 1);
            Ok(InvocationResult {
                invocation_id: _invocation_id.into(),
                status: InvocationStatus::Succeeded,
                output: json!({"stdout": "hello from v0"}),
            })
        }
    }

    /// Round 1 model call fails outright.
    struct FailFirstModel;

    impl Model for FailFirstModel {
        fn complete(&mut self, _turn: &Turn) -> Result<ModelOutput, String> {
            Err("round 1 model failed".into())
        }
    }

    /// Round 1 succeeds with an action; the round 2 model call fails after
    /// the Invocation has already been submitted.
    struct FailSecondModel;

    impl Model for FailSecondModel {
        fn complete(&mut self, turn: &Turn) -> Result<ModelOutput, String> {
            if turn.follow_ups.is_empty() {
                Ok(ModelOutput {
                    text: String::new(),
                    action: Some(Action {
                        tool: "c17".into(),
                        arguments: json!({"command": "echo", "args": ["x"]}),
                    }),
                })
            } else {
                Err("round 2 model failed".into())
            }
        }
    }

    #[test]
    fn first_model_call_failure_propagates_without_invocation() {
        let port = CountingPort { submits: std::cell::Cell::new(0) };
        let mut runtime = RuntimeLoop::new(port, FailFirstModel);
        let tool = Tool {
            name: "c17".into(),
            description: "run a command".into(),
            parameters: json!({"type": "object"}),
        };
        let err = runtime.run("hello", &[tool]).expect_err("must fail");
        assert!(err.contains("round 1 model failed"), "got: {err}");
        assert_eq!(runtime.port.submits.get(), 0, "no invocation may be submitted");
    }

    #[test]
    fn second_model_call_failure_propagates_without_retrying_invocation() {
        let port = CountingPort { submits: std::cell::Cell::new(0) };
        let mut runtime = RuntimeLoop::new(port, FailSecondModel);
        let tool = Tool {
            name: "c17".into(),
            description: "run a command".into(),
            parameters: json!({"type": "object"}),
        };
        let err = runtime.run("hello", &[tool]).expect_err("must fail");
        assert!(err.contains("round 2 model failed"), "got: {err}");
        assert_eq!(runtime.port.submits.get(), 1, "exactly one Invocation, never retried");
    }

    // ── bounded continuous loop scenarios ────────────────────────────────

    /// A port that records every submission's invocation id and count.
    struct RecordingPort {
        submissions: std::cell::Cell<u32>,
        invocation_ids: std::cell::RefCell<Vec<String>>,
    }

    impl InvocationPort for RecordingPort {
        fn submit(
            &self,
            invocation_id: &str,
            _capability_ref: &str,
            _arguments: Value,
        ) -> Result<InvocationResult, String> {
            self.submissions.set(self.submissions.get() + 1);
            self.invocation_ids.borrow_mut().push(invocation_id.to_string());
            Ok(InvocationResult {
                invocation_id: invocation_id.into(),
                status: InvocationStatus::Succeeded,
                output: json!({"stdout": format!("out-{}", self.submissions.get())}),
            })
        }
    }

    enum Step {
        Action(&'static str),
        Answer(&'static str),
        Fail(&'static str),
    }

    /// A scripted model: returns each preset step in order and records the
    /// follow-up texts it saw on every turn (proves result accumulation).
    struct SequenceModel {
        steps: Vec<Step>,
        seen: Vec<Vec<String>>,
    }

    impl Model for SequenceModel {
        fn complete(&mut self, turn: &Turn) -> Result<ModelOutput, String> {
            self.seen
                .push(turn.follow_ups.iter().map(|f| f.text.clone()).collect());
            match self.steps.remove(0) {
                Step::Action(tool) => Ok(ModelOutput {
                    text: String::new(),
                    action: Some(Action {
                        tool: tool.into(),
                        arguments: json!({}),
                    }),
                }),
                Step::Answer(text) => Ok(ModelOutput {
                    text: text.into(),
                    action: None,
                }),
                Step::Fail(msg) => Err(msg.into()),
            }
        }
    }

    fn test_tool() -> Tool {
        Tool {
            name: "c17".into(),
            description: "run a command".into(),
            parameters: json!({"type": "object"}),
        }
    }

    #[test]
    fn loop_answers_directly_without_any_tool() {
        let port = RecordingPort {
            submissions: std::cell::Cell::new(0),
            invocation_ids: std::cell::RefCell::new(vec![]),
        };
        let model = SequenceModel {
            steps: vec![Step::Answer("no tools needed")],
            seen: vec![],
        };
        let mut runtime = RuntimeLoop::with_max_tool_calls(port, model, 8);
        let reply = runtime.run("hello", &[test_tool()]).expect("loop");
        assert_eq!(reply, "no tools needed");
        assert_eq!(runtime.port.submissions.get(), 0, "zero tool calls");
    }

    #[test]
    fn loop_runs_two_tools_and_third_turn_sees_both_results() {
        let port = RecordingPort {
            submissions: std::cell::Cell::new(0),
            invocation_ids: std::cell::RefCell::new(vec![]),
        };
        let model = SequenceModel {
            steps: vec![
                Step::Action("tool_a"),
                Step::Action("tool_b"),
                Step::Answer("final"),
            ],
            seen: vec![],
        };
        let mut runtime = RuntimeLoop::with_max_tool_calls(port, model, 3);
        let reply = runtime.run("hello", &[test_tool()]).expect("loop");
        assert_eq!(reply, "final");
        assert_eq!(runtime.port.submissions.get(), 2, "exactly two tool calls");
        let ids = runtime.port.invocation_ids.borrow();
        assert_ne!(ids[0], ids[1], "the two Invocations must have distinct ids");
        // Third model turn must see BOTH prior results, in order.
        assert_eq!(runtime.model.seen.len(), 3);
        assert!(runtime.model.seen[0].is_empty());
        assert_eq!(runtime.model.seen[1].len(), 1, "turn 2 sees result A");
        assert!(runtime.model.seen[1][0].contains("out-1"), "got: {:?}", runtime.model.seen[1]);
        assert_eq!(runtime.model.seen[2].len(), 2, "turn 3 sees results A+B");
        assert!(runtime.model.seen[2][0].contains("out-1"), "got: {:?}", runtime.model.seen[2]);
        assert!(runtime.model.seen[2][1].contains("out-2"), "got: {:?}", runtime.model.seen[2]);
    }

    #[test]
    fn loop_succeeds_when_answer_comes_exactly_at_limit() {
        // max_tool_calls=2: tools A and B execute, then the model may still
        // summarize once — the "last tool must be followed by a summary"
        // guard.
        let port = RecordingPort {
            submissions: std::cell::Cell::new(0),
            invocation_ids: std::cell::RefCell::new(vec![]),
        };
        let model = SequenceModel {
            steps: vec![
                Step::Action("tool_a"),
                Step::Action("tool_b"),
                Step::Answer("final after B"),
            ],
            seen: vec![],
        };
        let mut runtime = RuntimeLoop::with_max_tool_calls(port, model, 2);
        let reply = runtime.run("hello", &[test_tool()]).expect("loop");
        assert_eq!(reply, "final after B");
        assert_eq!(runtime.port.submissions.get(), 2);
    }

    #[test]
    fn loop_fails_explicitly_when_limit_exceeded_without_executing() {
        let port = RecordingPort {
            submissions: std::cell::Cell::new(0),
            invocation_ids: std::cell::RefCell::new(vec![]),
        };
        let model = SequenceModel {
            steps: vec![
                Step::Action("tool_a"),
                Step::Action("tool_b"),
                Step::Action("tool_c"),
            ],
            seen: vec![],
        };
        let mut runtime = RuntimeLoop::with_max_tool_calls(port, model, 2);
        let err = runtime.run("hello", &[test_tool()]).expect_err("must fail at the limit");
        assert!(
            err.contains("tool_call_limit_reached"),
            "must be an explicit limit failure, got: {err}"
        );
        assert_eq!(
            runtime.port.submissions.get(),
            2,
            "tool C must NOT execute; total stays at 2"
        );
    }

    #[test]
    fn loop_never_reruns_executed_tools_after_model_failure() {
        let port = RecordingPort {
            submissions: std::cell::Cell::new(0),
            invocation_ids: std::cell::RefCell::new(vec![]),
        };
        let model = SequenceModel {
            steps: vec![
                Step::Action("tool_a"),
                Step::Action("tool_b"),
                Step::Fail("second tool round failed"),
            ],
            seen: vec![],
        };
        let mut runtime = RuntimeLoop::with_max_tool_calls(port, model, 8);
        let err = runtime.run("hello", &[test_tool()]).expect_err("must fail");
        assert!(err.contains("second tool round failed"), "got: {err}");
        assert_eq!(
            runtime.port.submissions.get(),
            2,
            "already-executed tools are never re-executed"
        );
    }
}
