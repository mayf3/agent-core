use super::{
    audit_tool_call, parse_tool_call, tool_call_id_hash, ToolCallResult, ToolNameMap, ToolNameMode,
};
use serde_json::json;

fn response(tool_call: serde_json::Value) -> serde_json::Value {
    json!({"choices": [{"message": {"tool_calls": [tool_call]}}]})
}

fn passthrough() -> ToolNameMode {
    ToolNameMode::Passthrough
}

fn indexed() -> ToolNameMode {
    let mut m = ToolNameMap::new();
    m.insert("fn_0".into(), "time.now".into());
    m.insert("fn_1".into(), "session.recall_recent".into());
    ToolNameMode::IndexedMapping(m)
}

// === Passthrough mode (GLM/OpenAI) ===

#[test]
fn passthrough_uses_provider_name_as_is() {
    let r = parse_tool_call(
        &response(json!({
            "id": "x", "function": {"name": "time.now", "arguments": "{}"}
        })),
        &passthrough(),
    );
    let ToolCallResult::Valid(c) = &r else {
        panic!("expected valid")
    };
    assert_eq!(c.operation, "time.now");
}

#[test]
fn passthrough_unknown_name_passes_through() {
    // Passthrough does no map lookup — any provider name is accepted.
    let r = parse_tool_call(
        &response(json!({
            "id": "x", "function": {"name": "anything_goes", "arguments": "{}"}
        })),
        &passthrough(),
    );
    let ToolCallResult::Valid(c) = &r else {
        panic!("expected valid")
    };
    assert_eq!(c.operation, "anything_goes");
}

// === IndexedMapping mode (DeepSeek-like) ===

#[test]
fn indexed_encoded_name_is_resolved_via_map() {
    let r = parse_tool_call(
        &response(json!({
            "id": "x", "function": {"name": "fn_0", "arguments": "{}"}
        })),
        &indexed(),
    );
    let ToolCallResult::Valid(c) = &r else {
        panic!("expected valid")
    };
    assert_eq!(c.operation, "time.now");
}

#[test]
fn indexed_forged_name_not_in_map_is_malformed() {
    let r = parse_tool_call(
        &response(json!({
            "id": "x", "function": {"name": "fn_99", "arguments": "{}"}
        })),
        &indexed(),
    );
    assert!(matches!(r, ToolCallResult::Malformed(_)));
}

#[test]
fn indexed_names_dont_collide() {
    let m = indexed();
    let r0 = parse_tool_call(
        &response(json!({
            "id": "a", "function": {"name": "fn_0", "arguments": "{}"}
        })),
        &m,
    );
    let r1 = parse_tool_call(
        &response(json!({
            "id": "b", "function": {"name": "fn_1", "arguments": "{}"}
        })),
        &m,
    );
    let ToolCallResult::Valid(c0) = &r0 else {
        panic!("fn_0")
    };
    let ToolCallResult::Valid(c1) = &r1 else {
        panic!("fn_1")
    };
    assert_eq!(c0.operation, "time.now");
    assert_eq!(c1.operation, "session.recall_recent");
}

#[test]
fn indexed_empty_tools_still_rejects_forged_name() {
    // IndexedMapping with an empty map: no names were exposed.
    let empty_indexed = ToolNameMode::IndexedMapping(ToolNameMap::new());
    let r = parse_tool_call(
        &response(json!({
            "id": "x", "function": {"name": "fn_0", "arguments": "{}"}
        })),
        &empty_indexed,
    );
    assert!(matches!(r, ToolCallResult::Malformed(_)));
}

// === Structural malformed tests (all modes) ===

#[test]
fn malformed_shapes_rejected_in_passthrough() {
    for case in [
        json!({"id": "x"}),
        json!({"id": "x", "function": null}),
        json!({"function": {"name": "time.now", "arguments": "{}"}}),
        json!({"id": "", "function": {"name": "time.now", "arguments": "{}"}}),
        json!({"id": 7, "function": {"name": "time.now", "arguments": "{}"}}),
        json!({"id": "x", "function": {"arguments": "{}"}}),
        json!({"id": "x", "function": {"name": "", "arguments": "{}"}}),
        json!({"id": "x", "function": {"name": "time.now"}}),
        json!({"id": "x", "function": {"name": "time.now", "arguments": 7}}),
        json!({"id": "x", "function": {"name": "time.now", "arguments": "[1]"}}),
        json!({"id": "x", "function": {"name": "time.now", "arguments": "{"}}),
    ] {
        let p = parse_tool_call(&response(case), &passthrough());
        assert!(matches!(p, ToolCallResult::Malformed(_)));
        assert_eq!(
            audit_tool_call(&p),
            json!({"malformed": "malformed_tool_call"})
        );
    }
}

// === Idempotency / audit ===

#[test]
fn provider_dto_hashes_id_once_and_bounds_unknown_operation_audit() {
    let raw_id = "provider-call-id";
    let raw_operation = "Authorization: Bearer hidden\n".repeat(100);
    let p = parse_tool_call(
        &response(json!({
            "id": raw_id,
            "function": {"name": raw_operation, "arguments": "{}"}
        })),
        &passthrough(),
    );
    let ToolCallResult::Valid(c) = &p else {
        panic!("expected valid")
    };
    assert_eq!(c.id, tool_call_id_hash(raw_id));
    assert_eq!(
        audit_tool_call(&p),
        json!({"operation": "unknown_operation", "id": c.id})
    );
    assert!(!audit_tool_call(&p).to_string().contains("Bearer"));
}
