use super::{
    parsing::{self, ParsedToolCall},
    tool_call_id_hash, EndpointChoice, ToolCallResult, ToolNameMap, ToolNameMode,
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

fn parse(value: &serde_json::Value, mode: &ToolNameMode) -> ToolCallResult {
    parsing::parse_tool_call(value, mode, EndpointChoice::Primary).tool_call_result
}

// === Passthrough mode (GLM/OpenAI) ===

#[test]
fn passthrough_uses_provider_name_as_is() {
    let r = parse(
        &response(json!({"id": "x", "function": {"name": "time.now", "arguments": "{}"}})),
        &passthrough(),
    );
    let ToolCallResult::Valid(c) = &r else {
        panic!("expected valid")
    };
    assert_eq!(c.operation, "time.now");
}

#[test]
fn passthrough_unknown_name_passes_through() {
    let r = parse(
        &response(json!({"id": "x", "function": {"name": "anything_goes", "arguments": "{}"}})),
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
    let r = parse(
        &response(json!({"id": "x", "function": {"name": "fn_0", "arguments": "{}"}})),
        &indexed(),
    );
    let ToolCallResult::Valid(c) = &r else {
        panic!("expected valid")
    };
    assert_eq!(c.operation, "time.now");
}

#[test]
fn indexed_forged_name_not_in_map_is_malformed() {
    let r = parse(
        &response(json!({"id": "x", "function": {"name": "fn_99", "arguments": "{}"}})),
        &indexed(),
    );
    assert!(matches!(r, ToolCallResult::Malformed(_)));
}

#[test]
fn indexed_names_dont_collide() {
    let m = indexed();
    let r0 = parse(
        &response(json!({"id": "a", "function": {"name": "fn_0", "arguments": "{}"}})),
        &m,
    );
    let r1 = parse(
        &response(json!({"id": "b", "function": {"name": "fn_1", "arguments": "{}"}})),
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
    let empty_indexed = ToolNameMode::IndexedMapping(ToolNameMap::new());
    let r = parse(
        &response(json!({"id": "x", "function": {"name": "fn_0", "arguments": "{}"}})),
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
        let p = parse(&response(case), &passthrough());
        assert!(matches!(p, ToolCallResult::Malformed(_)));
        assert_eq!(
            parsing::audit_tool_call(&p),
            json!({"malformed": "malformed_tool_call"})
        );
    }
}

// === Idempotency / audit ===

#[test]
fn provider_dto_hashes_id_once_and_bounds_unknown_operation_audit() {
    let raw_id = "provider-call-id";
    let raw_operation = "Authorization: Bearer hidden"; // short enough for wire name limit
    let p = parse(
        &response(json!({"id": raw_id, "function": {"name": raw_operation, "arguments": "{}"}})),
        &passthrough(),
    );
    let ToolCallResult::Valid(c) = &p else {
        panic!("expected valid")
    };
    assert_eq!(c.id, tool_call_id_hash(raw_id));
    assert_eq!(
        parsing::audit_tool_call(&p),
        json!({"operation": "unknown_operation", "id": c.id})
    );
    assert!(!parsing::audit_tool_call(&p).to_string().contains("Bearer"));
}

// === ProviderToolTurn provenance ===

#[test]
fn provider_turn_carries_raw_id_wire_name_and_endpoint() {
    let parsed = parsing::parse_tool_call(
        &response(json!({
            "id": "call_ds_123",
            "function": {"name": "fn_0", "arguments": "{\"limit\":5}"}
        })),
        &indexed(),
        EndpointChoice::Fallback,
    );
    assert!(matches!(parsed.tool_call_result, ToolCallResult::Valid(_)));
    let turn = parsed.provider_turn.expect("provider_turn present");
    assert_eq!(turn.endpoint, EndpointChoice::Fallback);
    assert_eq!(turn.provider_tool_call_id, "call_ds_123");
    assert_eq!(turn.wire_name, "fn_0");
    assert_eq!(turn.arguments_json, r#"{"limit":5}"#);
    assert_eq!(turn.canonical_operation, "time.now");
}

#[test]
fn provider_turn_absent_when_no_tool_call() {
    let parsed = parsing::parse_tool_call(
        &json!({"choices": [{"message": {"content": "hello"}}]}),
        &passthrough(),
        EndpointChoice::Primary,
    );
    assert!(matches!(parsed.tool_call_result, ToolCallResult::Absent));
    assert!(parsed.provider_turn.is_none());
}

#[test]
fn oversized_provider_id_is_malformed() {
    let huge_id = "x".repeat(300);
    let parsed = parsing::parse_tool_call(
        &response(json!({"id": huge_id, "function": {"name": "time.now", "arguments": "{}"}})),
        &passthrough(),
        EndpointChoice::Primary,
    );
    assert!(matches!(
        parsed.tool_call_result,
        ToolCallResult::Malformed(_)
    ));
    assert!(parsed.provider_turn.is_none());
}
