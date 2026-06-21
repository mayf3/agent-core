use super::{audit_tool_call, parse_tool_call, tool_call_id_hash, ToolCallResult, ToolNameMap};
use serde_json::json;

/// An empty map — used for non-deepseek endpoints where names pass through as-is.
fn empty_map() -> ToolNameMap {
    ToolNameMap::new()
}

/// A map simulating the deepseek fn_0/fn_1 encoding for `time.now` and
/// `session.recall_recent`.
fn deepseek_map() -> ToolNameMap {
    let mut m = ToolNameMap::new();
    m.insert("fn_0".into(), "time.now".into());
    m.insert("fn_1".into(), "session.recall_recent".into());
    m
}

fn response(tool_call: serde_json::Value) -> serde_json::Value {
    json!({"choices": [{"message": {"tool_calls": [tool_call]}}]})
}

#[test]
fn provider_dto_basic_valid_parse_with_empty_map() {
    let parsed = parse_tool_call(&response(json!({
        "id": "x",
        "function": {"name": "time.now", "arguments": "{}"}
    })), &empty_map());
    let ToolCallResult::Valid(call) = &parsed else {
        panic!("expected valid, got {parsed:?}");
    };
    assert_eq!(call.operation, "time.now");
}

#[test]
fn provider_dto_deepseek_encoded_name_is_resolved_via_map() {
    let parsed = parse_tool_call(&response(json!({
        "id": "x",
        "function": {"name": "fn_0", "arguments": "{}"}
    })), &deepseek_map());
    let ToolCallResult::Valid(call) = &parsed else {
        panic!("expected valid, got {parsed:?}");
    };
    assert_eq!(call.operation, "time.now");
}

#[test]
fn provider_dto_forged_name_not_in_map_is_malformed() {
    let parsed = parse_tool_call(&response(json!({
        "id": "x",
        "function": {"name": "fn_99", "arguments": "{}"}
    })), &deepseek_map());
    assert!(matches!(parsed, ToolCallResult::Malformed(_)));
}

#[test]
fn provider_dto_deepseek_names_dont_collide() {
    // fn_0 → time.now, fn_1 → session.recall_recent — distinct, no collision.
    let m = deepseek_map();
    let r0 = parse_tool_call(&response(json!({
        "id": "a", "function": {"name": "fn_0", "arguments": "{}"}
    })), &m);
    let r1 = parse_tool_call(&response(json!({
        "id": "b", "function": {"name": "fn_1", "arguments": "{}"}
    })), &m);
    let ToolCallResult::Valid(c0) = &r0 else { panic!("fn_0 failed") };
    let ToolCallResult::Valid(c1) = &r1 else { panic!("fn_1 failed") };
    assert_eq!(c0.operation, "time.now");
    assert_eq!(c1.operation, "session.recall_recent");
}

#[test]
fn provider_dto_rejects_every_structural_malformed_shape() {
    let cases = [
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
    ];
    for case in cases {
        let parsed = parse_tool_call(&response(case), &empty_map());
        assert!(matches!(parsed, ToolCallResult::Malformed(_)));
        assert_eq!(
            audit_tool_call(&parsed),
            json!({"malformed": "malformed_tool_call"})
        );
    }
}

#[test]
fn provider_dto_hashes_id_once_and_bounds_unknown_operation_audit() {
    let raw_id = "provider-call-id";
    let raw_operation = "Authorization: Bearer hidden\n".repeat(100);
    let parsed = parse_tool_call(&response(json!({
        "id": raw_id,
        "function": {"name": raw_operation, "arguments": "{}"}
    })), &empty_map());
    let ToolCallResult::Valid(call) = &parsed else {
        panic!("valid provider DTO expected");
    };
    assert_eq!(call.id, tool_call_id_hash(raw_id));
    assert_eq!(
        audit_tool_call(&parsed),
        json!({"operation": "unknown_operation", "id": call.id})
    );
    assert!(!audit_tool_call(&parsed).to_string().contains("Bearer"));
}

#[test]
fn provider_dto_empty_map_passes_name_through() {
    // When map is empty (non-deepseek endpoint), the provider name is used as-is.
    let parsed = parse_tool_call(&response(json!({
        "id": "x",
        "function": {"name": "time.now", "arguments": "{}"}
    })), &empty_map());
    let ToolCallResult::Valid(call) = &parsed else {
        panic!("expected valid, got {parsed:?}");
    };
    assert_eq!(call.operation, "time.now");
}
