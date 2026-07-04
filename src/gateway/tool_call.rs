//! Tool-call validation (Phase 2 tool-call execution MVP).
//!
//! Validates a model-emitted [`ToolCall`] against the operation catalog before
//! any adapter runs. The MVP allows only `ReadOnly` operations; `Write`
//! operations and unknown/generated operation names are rejected with a typed
//! [`ToolRejection`] (never a raw string match).
//!
//! See `docs/decisions/tool-call-execution-loop.md` §4.

use crate::domain::{InvocationId, InvocationIntent, RunId};
use crate::llm::ToolCall;
use crate::registry::snapshot::{RegistrySnapshot, Risk};

/// Typed, bounded reasons for rejecting a model tool call before capability
/// execution. Messages never include provider input or infrastructure errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolRejection {
    #[error("Tool call rejected: unknown operation.")]
    UnknownOperation,
    #[error("Tool call rejected: operation is not allowed.")]
    OperationNotAllowed,
    #[error("Tool call rejected: malformed arguments.")]
    MalformedArguments,
    #[error("Tool call rejected: invalid arguments.")]
    InvalidArguments,
    /// Invalid arguments with recoverable details from schema validation.
    /// The inner issue provides structured information for the model to self-correct.
    #[error("Tool call rejected: invalid arguments with details.")]
    InvalidArgumentsWithDetails(Box<crate::registry::schema::SchemaValidationIssue>),
    #[error("Tool call rejected: not permitted by policy.")]
    PolicyDenied,
    #[error("Tool call rejected: malformed tool call.")]
    MalformedToolCall,
    #[error("Tool call rejected: internal tool error.")]
    InternalToolError,
}

impl ToolRejection {
    pub(crate) fn category(&self) -> &'static str {
        match self {
            Self::UnknownOperation => "unknown_operation",
            Self::OperationNotAllowed => "operation_not_allowed",
            Self::MalformedArguments => "malformed_arguments",
            Self::InvalidArguments | Self::InvalidArgumentsWithDetails(_) => "invalid_arguments",
            Self::PolicyDenied => "policy_denied",
            Self::MalformedToolCall => "malformed_tool_call",
            Self::InternalToolError => "internal_tool_error",
        }
    }

    pub(crate) fn safe_message(&self) -> &'static str {
        match self {
            Self::UnknownOperation => "Tool call rejected: unknown operation.",
            Self::OperationNotAllowed => "Tool call rejected: operation is not allowed.",
            Self::MalformedArguments => "Tool call rejected: malformed arguments.",
            Self::InvalidArguments | Self::InvalidArgumentsWithDetails(_) => {
                "Tool call rejected: invalid arguments."
            }
            Self::PolicyDenied => "Tool call rejected: not permitted by policy.",
            Self::MalformedToolCall => "Tool call rejected: malformed tool call.",
            Self::InternalToolError => "Tool call rejected: internal tool error.",
        }
    }
}

/// Validate a model-emitted tool call and convert it into an
/// [`InvocationIntent`]. Returns a typed [`ToolRejection`] (without executing
/// anything) when:
/// - the operation is not in the provided registry snapshot (`UnknownOperation`);
/// - the operation is `Risk::Write` (`OperationNotAllowed`) — the MVP
///   restricts this path to `ReadOnly` only;
/// - the arguments are not a JSON object (`MalformedArguments`).
///
/// The idempotency key is scoped by trusted call position to make provider
/// `tool_call.id` collisions impossible within and across turns:
///
/// ```text
/// tool:{run_id}:{turn_index}:{tool_index}:{provider_id_digest}
/// ```
///
/// - `provider_id_digest` is the opaque, already-hashed provider id (`call.id`)
///   — it is hashed exactly once at the provider DTO boundary
///   (`parse_tool_call`); it is NOT re-hashed here.
/// - `turn_index` + `tool_index` are trusted, monotonic call positions threaded
///   from the runtime tool loop. Same (run,turn,index) replays stably; a
///   provider that reuses the same `tool_call.id` across turns or calls no
///   longer collides.
pub fn validate_tool_call(
    call: &ToolCall,
    run_id: &RunId,
    turn_index: usize,
    tool_index: usize,
    snapshot: &RegistrySnapshot,
) -> Result<InvocationIntent, ToolRejection> {
    let Some(spec) = snapshot.lookup(&call.operation) else {
        return Err(ToolRejection::UnknownOperation);
    };
    if spec.risk != Risk::ReadOnly {
        return Err(ToolRejection::OperationNotAllowed);
    }
    if !call.arguments.is_object() {
        return Err(ToolRejection::MalformedArguments);
    }
    // `call.id` is the opaque, single-hashed provider digest (see parse_tool_call
    // in llm/mod.rs). It is treated as an internal provider id here — never
    // re-hashed, never written raw.
    let idempotency_key = format!(
        "tool:{}:{}:{}:{}",
        run_id.0, turn_index, tool_index, call.id
    );
    Ok(InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run_id.clone(),
        operation: call.operation.clone(),
        arguments: call.arguments.clone(),
        idempotency_key: Some(idempotency_key),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(op: &str) -> ToolCall {
        ToolCall {
            id: "hashed_call_1".to_string(),
            operation: op.to_string(),
            arguments: json!({}),
        }
    }

    fn snap() -> crate::registry::snapshot::RegistrySnapshot {
        crate::registry::snapshot::test_snapshot()
    }

    #[test]
    fn accepts_valid_readonly_operation() {
        let run_id = RunId::new();
        let intent = validate_tool_call(&call("system.status"), &run_id, 0, 0, &snap()).unwrap();
        assert_eq!(intent.operation, "system.status");
        // Key composition: tool:{run_id}:{turn}:{index}:{provider_digest}
        let expected_key = format!("tool:{}:{}:{}:{}", run_id.0, 0, 0, "hashed_call_1");
        assert_eq!(
            intent.idempotency_key.as_deref(),
            Some(expected_key.as_str())
        );
    }

    #[test]
    fn same_provider_id_different_run_produces_different_keys() {
        let c = call("system.status");
        let snap = crate::registry::snapshot::test_snapshot();
        let intent_a = validate_tool_call(&c, &RunId::new(), 0, 0, &snap).unwrap();
        let intent_b = validate_tool_call(&c, &RunId::new(), 0, 0, &snap).unwrap();
        assert_ne!(
            intent_a.idempotency_key, intent_b.idempotency_key,
            "same provider_id + different run_id must produce different keys"
        );
    }

    #[test]
    fn same_run_same_turn_same_index_is_stable() {
        let c = call("system.status");
        let run = RunId::new();
        let s = snap();
        let intent_1 = validate_tool_call(&c, &run, 0, 0, &s).unwrap();
        let intent_2 = validate_tool_call(&c, &run, 0, 0, &s).unwrap();
        assert_eq!(
            intent_1.idempotency_key, intent_2.idempotency_key,
            "same (run,turn,index) must be stable"
        );
    }

    #[test]
    fn same_run_different_turn_produces_different_keys() {
        // Provider reusing the same tool_call.id across turns must NOT collide.
        let c = call("system.status");
        let run = RunId::new();
        let s = snap();
        let turn_0 = validate_tool_call(&c, &run, 0, 0, &s).unwrap();
        let turn_1 = validate_tool_call(&c, &run, 1, 0, &s).unwrap();
        assert_ne!(
            turn_0.idempotency_key, turn_1.idempotency_key,
            "different turn must produce different keys"
        );
    }

    #[test]
    fn same_run_same_turn_different_index_produces_different_keys() {
        // Multiple tool calls in the same turn must NOT collide.
        let c = call("system.status");
        let run = RunId::new();
        let s = snap();
        let idx_0 = validate_tool_call(&c, &run, 0, 0, &s).unwrap();
        let idx_1 = validate_tool_call(&c, &run, 0, 1, &s).unwrap();
        assert_ne!(
            idx_0.idempotency_key, idx_1.idempotency_key,
            "different index must produce different keys"
        );
    }

    #[test]
    fn raw_provider_id_not_in_idempotency_key() {
        // In the real flow parse_tool_call hashes the raw ID before putting it
        // in ToolCall.id — so this test also hashes the raw ID first.
        let raw_id = "provider_id_raw_12345";
        let hashed = crate::llm::tool_call_id_hash(raw_id);
        let c = ToolCall {
            id: hashed.clone(),
            operation: "system.status".to_string(),
            arguments: json!({}),
        };
        let s = snap();
        let intent = validate_tool_call(&c, &RunId::new(), 0, 0, &s).unwrap();
        let key = intent.idempotency_key.unwrap();
        assert!(
            !key.contains(raw_id),
            "raw provider ID must not leak into key"
        );
        assert!(key.starts_with("tool:"), "key should start with tool:");
        assert!(
            key.contains(&hashed),
            "key should contain the hashed provider ID"
        );
    }

    #[test]
    fn rejects_unknown_operation_typed() {
        let err =
            validate_tool_call(&call("shell.exec"), &RunId::new(), 0, 0, &snap()).unwrap_err();
        assert_eq!(err, ToolRejection::UnknownOperation);
        assert_eq!(err.category(), "unknown_operation");
    }

    #[test]
    fn rejects_write_operation_typed() {
        let err = validate_tool_call(&call("feishu.send_message"), &RunId::new(), 0, 0, &snap())
            .unwrap_err();
        assert_eq!(err, ToolRejection::OperationNotAllowed);
        assert_eq!(err.category(), "operation_not_allowed");
    }

    #[test]
    fn rejects_non_object_arguments_typed() {
        let mut c = call("system.status");
        c.arguments = json!("not-an-object");
        let err = validate_tool_call(&c, &RunId::new(), 0, 0, &snap()).unwrap_err();
        assert_eq!(err, ToolRejection::MalformedArguments);
        assert_eq!(err.category(), "malformed_arguments");
    }

    // --- tool_call_id_hash edge cases ---

    #[test]
    fn hash_control_chars_does_not_panic() {
        let long_id = format!("call_{}", "a".repeat(250));
        let ids = vec![
            "call\nwith\nnewlines",
            "call\twith\ttab",
            "call/with/path/separators",
            "call\\with\\backslash",
            "call.with.dots",
            "call🔥unicode",
            "call\u{0000}null",
            &long_id,
        ];
        let hashes: Vec<String> = ids
            .iter()
            .map(|id| crate::llm::tool_call_id_hash(id))
            .collect();
        for h in &hashes {
            assert_eq!(h.len(), 64, "hash should be 64 hex chars");
            assert!(
                h.chars().all(|c| c.is_ascii_hexdigit()),
                "hash should be hex: {h}"
            );
        }
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(hashes[i], hashes[j], "distinct inputs should not collide");
            }
        }
        let repeat = crate::llm::tool_call_id_hash("call\nwith\nnewlines");
        assert_eq!(hashes[0], repeat, "hash should be deterministic");
    }

    #[test]
    fn hash_provides_bounded_output() {
        let long_input = "x".repeat(10_000);
        let h = crate::llm::tool_call_id_hash(&long_input);
        assert_eq!(h.len(), 64);
    }
}
