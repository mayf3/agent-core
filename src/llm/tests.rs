use super::{audit_tool_call, parse_tool_call, tool_call_id_hash, ToolCallResult};
use serde_json::json;

fn response(tool_call: serde_json::Value) -> serde_json::Value {
    json!({"choices": [{"message": {"tool_calls": [tool_call]}}]})
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
        let parsed = parse_tool_call(&response(case));
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
    })));
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
