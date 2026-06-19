//! Tool-call validation (Phase 2 tool-call execution MVP).
//!
//! Validates a model-emitted [`ToolCall`] against the operation catalog before
//! any adapter runs. The MVP allows only `ReadOnly` operations (`time.now`);
//! `Write` operations and unknown/generated operation names are rejected.
//!
//! See `docs/decisions/tool-call-execution-loop.md` §4.

use crate::domain::operation::{self, Risk};
use crate::domain::{InvocationId, InvocationIntent, RunId};
use crate::llm::ToolCall;
use anyhow::{bail, Result};

/// Validate a model-emitted tool call and convert it into an
/// [`InvocationIntent`]. Returns an error (without executing anything) when:
/// - the operation is not in the catalog (`UnknownOperation`);
/// - the operation is `Risk::Write` (`WriteOperationNotAllowed`) — the MVP
///   restricts this path to `ReadOnly` only;
/// - the arguments are not a JSON object (`InvalidArguments`).
///
/// The resulting intent is associated with the current run (Option B in the
/// design doc) and carries an idempotency key seeded by the tool-call id.
pub fn validate_tool_call(call: &ToolCall, run_id: &RunId) -> Result<InvocationIntent> {
    let spec = operation::lookup(&call.operation)
        .ok_or_else(|| anyhow::anyhow!("unknown_operation: {}", call.operation))?;
    if spec.risk != Risk::ReadOnly {
        bail!(
            "write_operation_not_allowed: {} is a Write operation; the MVP tool-call path is ReadOnly-only",
            call.operation
        );
    }
    if !call.arguments.is_object() {
        bail!(
            "invalid_arguments: arguments for {} must be a JSON object",
            call.operation
        );
    }
    Ok(InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run_id.clone(),
        operation: call.operation.clone(),
        arguments: call.arguments.clone(),
        idempotency_key: Some(format!("tool:{}", call.id)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(op: &str) -> ToolCall {
        ToolCall {
            id: crate::llm::tool_call_id_hash("call_1"),
            operation: op.to_string(),
            arguments: json!({}),
        }
    }

    #[test]
    fn accepts_valid_readonly_operation() {
        let intent = validate_tool_call(&call("time.now"), &RunId::new()).unwrap();
        assert_eq!(intent.operation, "time.now");
        let expected_key = format!("tool:{}", crate::llm::tool_call_id_hash("call_1"));
        assert_eq!(
            intent.idempotency_key.as_deref(),
            Some(expected_key.as_str())
        );
    }

    #[test]
    fn rejects_unknown_operation() {
        let err = validate_tool_call(&call("shell.exec"), &RunId::new()).unwrap_err();
        assert!(err.to_string().contains("unknown_operation"));
    }

    #[test]
    fn rejects_write_operation() {
        let err = validate_tool_call(&call("feishu.send_message"), &RunId::new()).unwrap_err();
        assert!(err.to_string().contains("write_operation_not_allowed"));
    }

    #[test]
    fn rejects_non_object_arguments() {
        let mut c = call("time.now");
        c.arguments = json!("not-an-object");
        let err = validate_tool_call(&c, &RunId::new()).unwrap_err();
        assert!(err.to_string().contains("invalid_arguments"));
    }
}
