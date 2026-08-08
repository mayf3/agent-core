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

/// A model that speaks in turns.
pub trait Model {
    fn complete(&mut self, turn: Turn) -> ModelOutput;
}

/// The V0 loop: one model turn, one action through the port, the result
/// comes back as a follow-up for the next model turn.
pub struct RuntimeLoop<P: InvocationPort, M: Model> {
    port: P,
    model: M,
}

impl<P: InvocationPort, M: Model> RuntimeLoop<P, M> {
    pub fn new(port: P, model: M) -> Self {
        Self { port, model }
    }

    pub fn run(&mut self, user_text: &str, tool_view: &[Tool]) -> Result<String, String> {
        let first = Turn {
            user_text: user_text.to_string(),
            tool_view: tool_view.to_vec(),
            follow_ups: vec![],
        };
        let out = self.model.complete(first);
        let Some(action) = out.action else {
            return Ok(out.text);
        };
        // The loop generates a stable id for this call before submitting.
        let invocation_id = format!("inv_{}", uuid::Uuid::new_v4().simple());
        let result = self.port.submit(&invocation_id, &action.tool, action.arguments)?;
        let follow_up = ToolResult {
            tool: action.tool,
            text: render(&result),
        };
        let second = Turn {
            user_text: user_text.to_string(),
            tool_view: tool_view.to_vec(),
            follow_ups: vec![follow_up],
        };
        let final_out = self.model.complete(second);
        Ok(final_out.text)
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
        fn complete(&mut self, turn: Turn) -> ModelOutput {
            if self.round == 0 {
                self.round += 1;
                ModelOutput {
                    text: String::new(),
                    action: Some(Action {
                        tool: "c17".into(),
                        arguments: json!({"command": "echo", "args": ["x"]}),
                    }),
                }
            } else {
                let seen = turn
                    .follow_ups
                    .first()
                    .map(|f| f.text.clone())
                    .unwrap_or_default();
                ModelOutput {
                    text: format!("final reply: {seen}"),
                    action: None,
                }
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
            fn complete(&mut self, _turn: Turn) -> ModelOutput {
                ModelOutput { text: "no tools needed".into(), action: None }
            }
        }
        let port = FakePort;
        let mut runtime = RuntimeLoop::new(port, ReplyOnly);
        let reply = runtime.run("hello", &[]).expect("loop");
        assert_eq!(reply, "no tools needed");
    }
}
