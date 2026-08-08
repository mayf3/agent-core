//! Minimal canary Agent loop.
//!
//! The ONLY outside semantics this loop knows:
//!
//! ```text
//! submit(invocation_id, capability_ref, args) -> invocation_result
//! ```
//!
//! It intentionally knows nothing about the older world: no lifecycle
//! objects, no authorization, no pinned capability view, no approval
//! bookkeeping, no capability directory, no provider details, no
//! channel/product specifics. It holds a narrow port and a model that
//! speaks in plain turns; everything else lives in the compatibility
//! adapter in the sibling module.

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

/// A tool this loop offers to the model (its own view).
pub struct CanaryTool {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// The model decided to take one action through the port.
pub struct CanaryAction {
    pub tool: String,
    pub arguments: Value,
}

/// One completed action fed back to the model as plain text.
pub struct CanaryToolResult {
    pub tool: String,
    pub text: String,
}

/// Everything a model needs for one turn.
pub struct CanaryTurn {
    pub user_text: String,
    pub tool_view: Vec<CanaryTool>,
    pub follow_ups: Vec<CanaryToolResult>,
}

pub struct CanaryModelOutput {
    pub text: String,
    pub action: Option<CanaryAction>,
}

/// A model that speaks in turns (deterministic in the canary).
pub trait CanaryModel {
    fn complete(&mut self, turn: CanaryTurn) -> CanaryModelOutput;
}

/// The test capability reference this loop offers.
pub const C17: &str = "C17";

/// The tool view this loop offers for C17 (its own contract).
pub fn c17_tool() -> CanaryTool {
    CanaryTool {
        name: C17,
        description: "run a harmless command inside an isolated workspace",
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {"type": "string"},
                "command": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}},
                "timeout_seconds": {"type": "integer"}
            },
            "required": ["workspace_id", "command"]
        }),
    }
}

/// The canary loop: one model turn, one action through the port, the
/// result comes back as a follow-up for the next model turn.
pub struct CanaryRuntimeLoop<P: InvocationPort, M: CanaryModel> {
    port: P,
    model: M,
}

impl<P: InvocationPort, M: CanaryModel> CanaryRuntimeLoop<P, M> {
    pub fn new(port: P, model: M) -> Self {
        Self { port, model }
    }

    pub fn run(&mut self, user_text: &str) -> Result<String, String> {
        let first = CanaryTurn {
            user_text: user_text.to_string(),
            tool_view: vec![c17_tool()],
            follow_ups: vec![],
        };
        let out = self.model.complete(first);
        let Some(action) = out.action else {
            return Ok(out.text);
        };
        // The loop generates a stable id for this call before submitting.
        let invocation_id = format!("inv_canary_{}", uuid::Uuid::new_v4().simple());
        let result = self.port.submit(&invocation_id, &action.tool, action.arguments)?;
        let follow_up = CanaryToolResult {
            tool: action.tool,
            text: render(&result),
        };
        let second = CanaryTurn {
            user_text: user_text.to_string(),
            tool_view: vec![c17_tool()],
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
