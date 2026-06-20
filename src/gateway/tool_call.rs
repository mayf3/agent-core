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
    // The idempotency key MUST be scoped to the run to prevent cross-Run
    // collisions when a provider reuses the same tool_call.id across
    // different runs. Format: tool:{run_id}:{hashed_provider_id}.
    // The raw provider ID is never stored directly — only its hash.
    // call.id is ALREADY the hashed value (see parse_tool_call in llm/mod.rs).
    // DO NOT hash it again.
    let idempotency_key = format!("tool:{}:{}", run_id.0, call.id);
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
            id: "call_1".to_string(),
            operation: op.to_string(),
            arguments: json!({}),
        }
    }

    #[test]
    fn accepts_valid_readonly_operation() {
        let run_id = RunId::new();
        let intent = validate_tool_call(&call("time.now"), &run_id).unwrap();
        assert_eq!(intent.operation, "time.now");
        // Key must be run-scoped: tool:{run_id}:{hashed_provider_id}
        // call.id ("call_1") is already hashed by parse_tool_call.
        let expected_key = format!("tool:{}:{}", run_id.0, "call_1");
        assert_eq!(
            intent.idempotency_key.as_deref(),
            Some(expected_key.as_str())
        );
    }

    #[test]
    fn same_provider_id_different_run_produces_different_keys() {
        // Two different runs with the same provider tool_call.id must NOT
        // produce the same idempotency key (cross-Run collision prevention).
        let c = call("time.now");
        let run_a = RunId::new();
        let run_b = RunId::new();
        let intent_a = validate_tool_call(&c, &run_a).unwrap();
        let intent_b = validate_tool_call(&c, &run_b).unwrap();
        assert_ne!(
            intent_a.idempotency_key, intent_b.idempotency_key,
            "same provider_id + different run_id must produce different keys"
        );
    }

    #[test]
    fn same_run_same_call_produces_stable_key() {
        // Replaying the same (run_id, tool_call.id) must produce the same key.
        let c = call("time.now");
        let run = RunId::new();
        let intent_1 = validate_tool_call(&c, &run).unwrap();
        let intent_2 = validate_tool_call(&c, &run).unwrap();
        assert_eq!(
            intent_1.idempotency_key, intent_2.idempotency_key,
            "same run_id + same call must be stable"
        );
    }

    #[test]
    fn raw_provider_id_not_in_idempotency_key() {
        // The raw provider ID (before hashing) must never appear in the key.
        // In the real flow, parse_tool_call hashes the raw ID before putting
        // it in ToolCall.id — so this test also hashes the raw ID first.
        let raw_id = "provider_id_raw_12345";
        let hashed = crate::llm::tool_call_id_hash(raw_id);
        let c = ToolCall {
            id: hashed.clone(),
            operation: "time.now".to_string(),
            arguments: json!({}),
        };
        let intent = validate_tool_call(&c, &RunId::new()).unwrap();
        let key = intent.idempotency_key.unwrap();
        assert!(
            !key.contains(raw_id),
            "raw provider ID must not leak into key"
        );
        assert!(key.starts_with("tool:"), "key should start with tool:");
        // The key should contain the hashed ID (not the raw one).
        assert!(
            key.contains(&hashed),
            "key should contain the hashed provider ID"
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

    // --- tool_call_id_hash edge cases ---

    #[test]
    fn hash_control_chars_does_not_panic() {
        // Control characters, newlines, path separators, unicode — all
        // valid UTF-8 that should produce a stable hash.
        let ids = vec![
            "call\nwith\nnewlines",
            "call\twith\ttab",
            "call/with/path/separators",
            "call\\with\\backslash",
            "call.with.dots",
            "call🔥unicode",
            "call\u{0000}null",
        ];
        // 256-char id
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
        // All hashes should be exactly 64 hex chars (SHA-256 digest length).
        for h in &hashes {
            assert_eq!(
                h.len(),
                64,
                "hash should be 64 hex chars, got {} for input",
                h.len()
            );
            assert!(
                h.chars().all(|c| c.is_ascii_hexdigit()),
                "hash should be hex: {}",
                h
            );
        }
        // Distinct inputs must produce distinct hashes.
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(hashes[i], hashes[j], "distinct inputs should not collide");
            }
        }
        // Same input produces same hash (deterministic).
        let repeat = crate::llm::tool_call_id_hash("call\nwith\nnewlines");
        assert_eq!(hashes[0], repeat, "hash should be deterministic");
    }

    #[test]
    fn hash_provides_bounded_output() {
        // Even a maximally long input produces a 64-char hex hash.
        let long_input = "x".repeat(10_000);
        let h = crate::llm::tool_call_id_hash(&long_input);
        assert_eq!(h.len(), 64);
    }
}
