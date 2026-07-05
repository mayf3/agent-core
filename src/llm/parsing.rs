use super::{
    tool_call_id_hash, EndpointChoice, ProviderToolTurn, ToolCall, ToolCallResult, ToolNameMode,
};
use serde_json::{json, Value};

pub(super) struct ParsedToolCall {
    pub(super) tool_call_result: ToolCallResult,
    pub(super) provider_turn: Option<ProviderToolTurn>,
}

const MAX_PROVIDER_ID_LEN: usize = 256;
const MAX_WIRE_NAME_LEN: usize = 128;
const MAX_ARGS_JSON_LEN: usize = 8192;

pub(super) fn parse_tool_call(
    value: &Value,
    mode: &ToolNameMode,
    choice: EndpointChoice,
) -> ParsedToolCall {
    let tool_call_json = match value.pointer("/choices/0/message/tool_calls/0") {
        Some(v) if !v.is_null() => v,
        _ => {
            return ParsedToolCall {
                tool_call_result: ToolCallResult::Absent,
                provider_turn: None,
            }
        }
    };
    let function = match tool_call_json.get("function") {
        Some(f) => f,
        None => {
            return ParsedToolCall {
                tool_call_result: ToolCallResult::Malformed("missing function block".to_string()),
                provider_turn: None,
            }
        }
    };
    let raw_id = match tool_call_json.get("id").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s,
        _ => {
            return ParsedToolCall {
                tool_call_result: ToolCallResult::Malformed("missing tool_call id".to_string()),
                provider_turn: None,
            }
        }
    };
    if raw_id.len() > MAX_PROVIDER_ID_LEN {
        return ParsedToolCall {
            tool_call_result: ToolCallResult::Malformed("provider id too long".to_string()),
            provider_turn: None,
        };
    }
    let id = tool_call_id_hash(raw_id);
    let raw_operation = match function.get("name").and_then(Value::as_str) {
        Some(n) if !n.trim().is_empty() => n,
        _ => {
            return ParsedToolCall {
                tool_call_result: ToolCallResult::Malformed("missing function name".to_string()),
                provider_turn: None,
            }
        }
    };
    if raw_operation.len() > MAX_WIRE_NAME_LEN {
        return ParsedToolCall {
            tool_call_result: ToolCallResult::Malformed("wire name too long".to_string()),
            provider_turn: None,
        };
    }
    let operation = match mode {
        ToolNameMode::Passthrough => raw_operation.to_string(),
        ToolNameMode::IndexedMapping(map) => match map.get(raw_operation) {
            Some(canonical) => canonical.clone(),
            None => {
                return ParsedToolCall {
                    tool_call_result: ToolCallResult::Malformed(
                        "unknown function name".to_string(),
                    ),
                    provider_turn: None,
                }
            }
        },
    };
    let arguments_str = match function.get("arguments").and_then(Value::as_str) {
        Some(s) => s,
        None => {
            return ParsedToolCall {
                tool_call_result: ToolCallResult::Malformed("missing arguments".to_string()),
                provider_turn: None,
            }
        }
    };
    if arguments_str.len() > MAX_ARGS_JSON_LEN {
        return ParsedToolCall {
            tool_call_result: ToolCallResult::Malformed("arguments too long".to_string()),
            provider_turn: None,
        };
    }
    let arguments_val = match serde_json::from_str::<Value>(arguments_str) {
        Ok(v) if v.is_object() => v,
        Ok(v) => {
            return ParsedToolCall {
                tool_call_result: ToolCallResult::Malformed(format!(
                    "arguments must be a JSON object, got {}",
                    type_name(&v)
                )),
                provider_turn: None,
            }
        }
        Err(e) => {
            return ParsedToolCall {
                tool_call_result: ToolCallResult::Malformed(format!(
                    "arguments JSON parse error: {e}"
                )),
                provider_turn: None,
            }
        }
    };
    // Capture reasoning_content from the message level (DeepSeek thinking mode).
    let reasoning_content = value
        .pointer("/choices/0/message/reasoning_content")
        .and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_str().map(|s| s.to_string())
            }
        });

    let provider_turn = ProviderToolTurn {
        endpoint: choice,
        provider_tool_call_id: raw_id.to_string(),
        wire_name: raw_operation.to_string(),
        canonical_operation: operation.clone(),
        arguments_json: arguments_str.to_string(),
        reasoning_content,
    };
    ParsedToolCall {
        tool_call_result: ToolCallResult::Valid(ToolCall {
            id,
            operation,
            arguments: arguments_val,
        }),
        provider_turn: Some(provider_turn),
    }
}

pub(super) fn audit_tool_call(tool_call: &ToolCallResult) -> Value {
    match tool_call {
        ToolCallResult::Valid(call) => json!({
            "operation": crate::domain::operation::lookup(&call.operation)
                .map(|spec| spec.name)
                .unwrap_or("unknown_operation"),
            "id": call.id,
        }),
        ToolCallResult::Malformed(_) => json!({ "malformed": "malformed_tool_call" }),
        ToolCallResult::Absent => Value::Null,
    }
}

pub(super) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
