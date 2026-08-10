use super::{
    materialized_request_body, parsing::{self},
    sanitize_usage, tool_call_id_hash, EndpointChoice, LlmInput, ModelEndpoint, ToolCallResult,
    ToolNameMap, ToolNameMode,
};
use crate::domain::{ContextBlock, ContextBlockKind};
use serde_json::json;

fn response(tool_call: serde_json::Value) -> serde_json::Value {
    json!({"choices": [{"message": {"tool_calls": [tool_call]}}]})
}

fn passthrough() -> ToolNameMode {
    ToolNameMode::Passthrough
}

fn indexed() -> ToolNameMode {
    let mut m = ToolNameMap::new();
    m.insert("fn_0".into(), "system.status".into());
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
        &response(json!({"id": "x", "function": {"name": "system.status", "arguments": "{}"}})),
        &passthrough(),
    );
    let ToolCallResult::Valid(c) = &r else {
        panic!("expected valid")
    };
    assert_eq!(c.operation, "system.status");
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
    assert_eq!(c.operation, "system.status");
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
    assert_eq!(c0.operation, "system.status");
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
        json!({"function": {"name": "system.status", "arguments": "{}"}}),
        json!({"id": "", "function": {"name": "system.status", "arguments": "{}"}}),
        json!({"id": 7, "function": {"name": "system.status", "arguments": "{}"}}),
        json!({"id": "x", "function": {"arguments": "{}"}}),
        json!({"id": "x", "function": {"name": "", "arguments": "{}"}}),
        json!({"id": "x", "function": {"name": "system.status"}}),
        json!({"id": "x", "function": {"name": "system.status", "arguments": 7}}),
        json!({"id": "x", "function": {"name": "system.status", "arguments": "[1]"}}),
        json!({"id": "x", "function": {"name": "system.status", "arguments": "{"}}),
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
    assert_eq!(turn.canonical_operation, "system.status");
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

// === reasoning_content tests (DeepSeek thinking mode) ===

fn response_with_reasoning(
    tool_call: serde_json::Value,
    reasoning_content: Option<&str>,
) -> serde_json::Value {
    let mut msg = json!({"tool_calls": [tool_call]});
    if let Some(rc) = reasoning_content {
        msg["reasoning_content"] = json!(rc);
    }
    json!({"choices": [{"message": msg}]})
}

#[test]
fn reasoning_content_with_tool_call_is_captured() {
    let value = response_with_reasoning(
        json!({"id": "call_1", "function": {"name": "system.status", "arguments": "{}"}}),
        Some("thinking step 1..."),
    );
    let parsed = parsing::parse_tool_call(&value, &passthrough(), EndpointChoice::Primary);
    assert!(matches!(parsed.tool_call_result, ToolCallResult::Valid(_)));
    let turn = parsed.provider_turn.expect("provider_turn present");
    assert_eq!(
        turn.reasoning_content,
        Some("thinking step 1...".to_string())
    );
}

#[test]
fn reasoning_content_empty_string_is_preserved() {
    let value = response_with_reasoning(
        json!({"id": "call_2", "function": {"name": "system.status", "arguments": "{}"}}),
        Some(""),
    );
    let parsed = parsing::parse_tool_call(&value, &passthrough(), EndpointChoice::Primary);
    let turn = parsed.provider_turn.expect("provider_turn present");
    assert_eq!(turn.reasoning_content, Some("".to_string()));
}

#[test]
fn reasoning_content_absent_when_not_in_response() {
    let value = response_with_reasoning(
        json!({"id": "call_3", "function": {"name": "system.status", "arguments": "{}"}}),
        None,
    );
    let parsed = parsing::parse_tool_call(&value, &passthrough(), EndpointChoice::Primary);
    let turn = parsed.provider_turn.expect("provider_turn present");
    assert_eq!(turn.reasoning_content, None);
}

#[test]
fn reasoning_content_null_is_treated_as_absent() {
    let value = json!({"choices": [{"message": {
        "reasoning_content": null,
        "tool_calls": [{"id": "call_4", "type": "function", "function": {"name": "system.status", "arguments": "{}"}}]
    }}]});
    let parsed = parsing::parse_tool_call(&value, &passthrough(), EndpointChoice::Primary);
    let turn = parsed.provider_turn.expect("provider_turn present");
    assert_eq!(turn.reasoning_content, None);
}

#[test]
fn oversized_provider_id_is_malformed() {
    let huge_id = "x".repeat(300);
    let parsed = parsing::parse_tool_call(
        &response(json!({"id": huge_id, "function": {"name": "system.status", "arguments": "{}"}})),
        &passthrough(),
        EndpointChoice::Primary,
    );
    assert!(matches!(
        parsed.tool_call_result,
        ToolCallResult::Malformed(_)
    ));
    assert!(parsed.provider_turn.is_none());
}

#[test]
fn usage_normalizes_cached_reasoning_cost_and_provider_extensions() {
    let usage = sanitize_usage(Some(&json!({
        "prompt_tokens": 120,
        "completion_tokens": 30,
        "total_tokens": 150,
        "prompt_tokens_details": {"cached_tokens": 40},
        "completion_tokens_details": {"reasoning_tokens": 12},
        "estimated_cost": 0.0042,
        "cache_write_tokens": 9,
        "provider_counters": {"batch_hits": 2}
    })));
    assert_eq!(usage["input_tokens"], 120);
    assert_eq!(usage["cached_input_tokens"], 40);
    assert_eq!(usage["output_tokens"], 30);
    assert_eq!(usage["reasoning_tokens"], 12);
    assert_eq!(usage["total_tokens"], 150);
    assert_eq!(usage["estimated_cost"], 0.0042);
    assert_eq!(usage["provider_usage_extensions"]["cache_write_tokens"], 9);
    assert_eq!(
        usage["provider_usage_extensions"]["provider_counters"]["batch_hits"],
        2
    );
}

#[test]
fn malformed_usage_and_secret_extensions_fail_closed() {
    let usage = sanitize_usage(Some(&json!({
        "prompt_tokens": "not-a-number",
        "completion_tokens": -3,
        "total_tokens": 1.5,
        "api_key": "sk-test-secret",
        "access_token": 12345,
        "provider_note": "prompt text must not be copied",
        "safe_counter": 7
    })));
    assert!(usage["input_tokens"].is_null());
    assert!(usage["output_tokens"].is_null());
    assert!(usage["total_tokens"].is_null());
    assert!(usage["estimated_cost"].is_null());
    let extensions = usage["provider_usage_extensions"].as_object().unwrap();
    assert_eq!(extensions.get("safe_counter"), Some(&json!(7)));
    assert!(!extensions.contains_key("api_key"));
    assert!(!extensions.contains_key("access_token"));
    assert!(!extensions.contains_key("provider_note"));
    assert!(!usage.to_string().contains("sk-test-secret"));
    assert!(!usage.to_string().contains("prompt text"));
}

// === Context wire roles ===

/// A full-block input: every kind present, so the wire-role classification
/// is asserted for all of them at once.
fn full_block_input() -> LlmInput {
    let content = |kind: ContextBlockKind, tag: &str| ContextBlock {
        kind,
        content: format!("content-{tag}"),
        source_ref: Some(format!("ref-{tag}")),
    };
    LlmInput {
        timeout_override_ms: None,
        blocks: vec![
            content(ContextBlockKind::RootSystem, "root"),
            content(ContextBlockKind::RuntimeContract, "contract"),
            content(ContextBlockKind::AgentProfile, "profile"),
            content(ContextBlockKind::ActiveSkill, "skill"),
            content(ContextBlockKind::SkillCatalog, "catalog"),
            content(ContextBlockKind::ToolCatalog, "tool-catalog"),
            content(ContextBlockKind::RecentMessages, "history"),
            content(ContextBlockKind::UserMessage, "user-block"),
            content(ContextBlockKind::ToolResult, "tool-result"),
            content(ContextBlockKind::SupplementalSystem, "supp-sys"),
            content(ContextBlockKind::SupplementalContext, "supp-ctx"),
        ],
        user_text: "the actual user question".into(),
        granted_operations: vec![],
        provider_tools: vec![],
        follow_ups: vec![],
    }
}

#[test]
fn supplemental_system_gets_system_authority_and_context_stays_non_system() {
    let input = full_block_input();
    let endpoint = ModelEndpoint::new("http://example.test".into(), "k".into(), "m".into());
    let (body, _mode) = materialized_request_body(&endpoint, &input);
    let messages = body["messages"].as_array().unwrap();

    // System message: Kernel instructions + provider SupplementalSystem.
    let system = messages.first().unwrap();
    assert_eq!(system["role"], "system");
    let system_text = system["content"].as_str().unwrap();
    for marker in [
        "## RootSystem",
        "## RuntimeContract",
        "## AgentProfile",
        "## ActiveSkill",
        "## SupplementalSystem",
    ] {
        assert!(system_text.contains(marker), "system must contain {marker}");
    }
    for marker in [
        "## SupplementalContext",
        "## SkillCatalog",
        "## ToolCatalog",
        "## RecentMessages",
        "## UserMessage",
        "## ToolResult",
    ] {
        assert!(
            !system_text.contains(marker),
            "system must NOT contain {marker}"
        );
    }

    // Non-system context: one plain user-role message carrying provider
    // material, history, and catalogs — never promoted to system.
    let context_idx = messages
        .iter()
        .position(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("## SupplementalContext")
        })
        .expect("non-system context user message");
    let context_text = messages[context_idx]["content"].as_str().unwrap();
    for marker in [
        "## SupplementalContext",
        "## SkillCatalog",
        "## ToolCatalog",
        "## RecentMessages",
    ] {
        assert!(
            context_text.contains(marker),
            "context message must contain {marker}"
        );
    }

    // The actual user text is a separate user message AFTER the context
    // message, verbatim; UserMessage/ToolResult blocks never appear.
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"], "the actual user question");
    assert!(
        context_idx < messages.len() - 1,
        "context message must precede the primary user message"
    );
}

#[test]
fn empty_non_system_context_keeps_single_user_message() {
    let input = LlmInput {
        timeout_override_ms: None,
        blocks: vec![ContextBlock {
            kind: ContextBlockKind::RootSystem,
            content: "root".into(),
            source_ref: None,
        }],
        user_text: "hi".into(),
        granted_operations: vec![],
        provider_tools: vec![],
        follow_ups: vec![],
    };
    let endpoint = ModelEndpoint::new("http://example.test".into(), "k".into(), "m".into());
    let (body, _mode) = materialized_request_body(&endpoint, &input);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "system + user only");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["content"], "hi");
}
