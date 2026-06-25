use crate::domain::ContextBlock;
use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;

pub trait LlmClient {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput>;
}

pub struct LlmInput {
    pub blocks: Vec<ContextBlock>,
    pub user_text: String,
    pub granted_operations: Vec<String>,
    /// Structured tool follow-up for the second round: the provider-side
    /// tool_call transcript (raw id, wire name, args) + bounded result content.
    /// When present, the LLM sends `role: assistant` + `role: tool` messages to
    /// the **source endpoint** that returned the tool call (sticky). This is
    /// Run-local state threaded through LlmInput — never shared client state.
    pub follow_up: Option<LlmFollowUp>,
}

pub struct LlmOutput {
    pub provider: String,
    pub model: String,
    pub content: String,
    pub journal_payload: Value,
    pub tool_call: ToolCallResult,
    /// When a tool call was parsed from the provider response, the raw
    /// provider-side metadata (endpoint, raw id, wire name, args JSON) is
    /// carried here so the Runtime can build an `LlmFollowUp` for the next
    /// round — without leaking raw ids/wire names into the Journal `ToolCall`.
    pub provider_turn: Option<ProviderToolTurn>,
}

#[derive(Debug, Clone)]
pub enum ToolCallResult {
    Absent,
    Valid(ToolCall),
    Malformed(String),
}

impl ToolCallResult {
    pub fn is_absent(&self) -> bool {
        matches!(self, ToolCallResult::Absent)
    }
}

pub fn tool_call_id_hash(provider_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(provider_id.as_bytes());
    hex::encode(hasher.finalize())
}

/// Kernel-authoritative, provider-agnostic tool call. Only the internal hashed
/// id, canonical operation, and parsed arguments live here. Raw provider id,
/// wire name, and raw arguments JSON travel in `ProviderToolTurn`.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub operation: String,
    pub arguments: Value,
}

/// Which endpoint returned the tool call. Determined at the actual HTTP request
/// site — never inferred from turn_index, model name, or URL substring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointChoice {
    Primary,
    Fallback,
}

/// Provider-side metadata for a single tool-call round. The raw provider id is
/// preserved verbatim (bounded) so the follow-up `role: tool` message can match
/// the provider's own `tool_call_id`. This never enters the Journal.
#[derive(Debug, Clone)]
pub struct ProviderToolTurn {
    pub endpoint: EndpointChoice,
    pub provider_tool_call_id: String,
    pub wire_name: String,
    pub canonical_operation: String,
    pub arguments_json: String,
}

/// A structured follow-up carried Run-locally through LlmInput: the provider
/// transcript of the first round + the bounded result content.
#[derive(Debug, Clone)]
pub struct LlmFollowUp {
    pub provider_turn: ProviderToolTurn,
    pub result_content: String,
}

#[derive(Debug, Clone)]
pub(crate) enum ToolNameMode {
    Passthrough,
    IndexedMapping(ToolNameMap),
}

pub(crate) type ToolNameMap = HashMap<String, String>;

impl<T: LlmClient + ?Sized> LlmClient for Box<T> {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        (**self).complete(input)
    }
}

pub struct LocalEchoLlm;
impl LlmClient for LocalEchoLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        Ok(LlmOutput {
            provider: "local".to_string(),
            model: "local-echo".to_string(),
            content: format!("收到：{}", input.user_text),
            tool_call: ToolCallResult::Absent,
            journal_payload: json!({
                "provider": "local",
                "model": "local-echo",
                "context_blocks": input.blocks.len(),
                "status": "ok",
            }),
            provider_turn: None,
        })
    }
}

pub struct OpenAiCompatibleLlm {
    pub(crate) primary: ModelEndpoint,
    pub(crate) fallback: Option<ModelEndpoint>,
    timeout: Duration,
}

impl OpenAiCompatibleLlm {
    pub fn new(base_url: String, api_key: String, model: String, timeout_ms: u64) -> Self {
        Self {
            primary: ModelEndpoint::new(base_url, api_key, model),
            fallback: None,
            timeout: Duration::from_millis(timeout_ms),
        }
    }
    pub fn with_fallback(mut self, base_url: String, api_key: String, model: String) -> Self {
        let endpoint = ModelEndpoint::new(base_url, api_key, model);
        if endpoint.is_configured() {
            self.fallback = Some(endpoint);
        }
        self
    }
    pub fn with_indexed_fallback(
        mut self,
        base_url: String,
        api_key: String,
        model: String,
    ) -> Self {
        let endpoint = ModelEndpoint::new(base_url, api_key, model).with_indexed_tool_name();
        if endpoint.is_configured() {
            self.fallback = Some(endpoint);
        }
        self
    }
    pub fn with_indexed_primary(mut self) -> Self {
        self.primary = self.primary.with_indexed_tool_name();
        self
    }
}

impl LlmClient for OpenAiCompatibleLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        // Sticky follow-up: if a follow_up names the source endpoint, request
        // ONLY that endpoint (do not cross providers).
        if let Some(fu) = &input.follow_up {
            let endpoint = match fu.provider_turn.endpoint {
                EndpointChoice::Primary => &self.primary,
                EndpointChoice::Fallback => match &self.fallback {
                    Some(fb) => fb,
                    None => &self.primary,
                },
            };
            return self.request_endpoint(endpoint, fu.provider_turn.endpoint, &input, fu)
                .map_err(anyhow::Error::msg);
        }
        // No follow-up: normal primary → fallback routing.
        if !self.primary.is_configured() {
            return Ok(match self.try_fallback(&input, "model_config_required") {
                Some(o) => o,
                None => config_required_output(&self.primary.model, input.blocks.len()),
            });
        }
        match self.request_endpoint(&self.primary, EndpointChoice::Primary, &input, &empty_followup(&input)) {
            Ok(output) => Ok(output),
            Err(error) => {
                if let Some(output) = self.try_fallback(&input, error.as_str()) {
                    return Ok(output);
                }
                Ok(request_failed_output(&self.primary.model, input.blocks.len(), error.as_str()))
            }
        }
    }
}

impl OpenAiCompatibleLlm {
    fn try_fallback(&self, input: &LlmInput, primary_error: &str) -> Option<LlmOutput> {
        let fallback = self.fallback.as_ref()?;
        let output =
            match self.request_endpoint(fallback, EndpointChoice::Fallback, input, &empty_followup(input)) {
                Ok(output) => output,
                Err(error) => request_failed_output(&fallback.model, input.blocks.len(), error.as_str()),
            };
        Some(mark_fallback(output, &self.primary.model, primary_error))
    }

    /// The single HTTP request site. `choice` is the authoritative endpoint
    /// identity — it is recorded into the `ProviderToolTurn` if a tool call is
    /// parsed, so the follow-up round is sticky to this exact endpoint.
    fn request_endpoint(
        &self,
        endpoint: &ModelEndpoint,
        choice: EndpointChoice,
        input: &LlmInput,
        follow_up: &LlmFollowUp,
    ) -> std::result::Result<LlmOutput, String> {
        let mut tools =
            crate::domain::operation::provider_tools_for_grants(&input.granted_operations);
        let tool_name_mode = match &endpoint.tool_name_mode {
            ToolNameMode::Passthrough => ToolNameMode::Passthrough,
            ToolNameMode::IndexedMapping(_) => {
                let mut map = ToolNameMap::new();
                for (idx, tool) in tools.iter_mut().enumerate() {
                    if let Some(name) = tool.pointer("/function/name").and_then(Value::as_str) {
                        let safe = format!("fn_{idx}");
                        map.insert(safe.clone(), name.to_string());
                        tool["function"]["name"] = json!(safe);
                    }
                }
                ToolNameMode::IndexedMapping(map)
            }
        };
        let mut messages: Vec<Value> = vec![
            json!({"role": "system", "content": serialize_system_context(&input.blocks)}),
            json!({"role": "user", "content": input.user_text}),
        ];
        // Structured follow-up transcript: assistant tool_calls + role:tool.
        let turn = &follow_up.provider_turn;
        messages.push(json!({
            "role": "assistant",
            "tool_calls": [{
                "id": turn.provider_tool_call_id,
                "type": "function",
                "function": {
                    "name": turn.wire_name,
                    "arguments": turn.arguments_json,
                }
            }]
        }));
        messages.push(json!({
            "role": "tool",
            "tool_call_id": turn.provider_tool_call_id,
            "content": follow_up.result_content,
        }));
        let body = json!({
            "model": endpoint.model,
            "messages": messages,
            "temperature": 0.2,
            "tools": tools,
            "tool_choice": "auto",
        });
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .build()
            .new_agent();
        let response = agent
            .post(&endpoint.chat_completions_url())
            .header("authorization", &format!("Bearer {}", endpoint.api_key))
            .header("content-type", "application/json")
            .send_json(body);
        let value = match response {
            Ok(mut response) => response
                .body_mut()
                .read_json::<Value>()
                .map_err(|_| "model_response_parse_failed".to_string())?,
            Err(ureq::Error::StatusCode(code)) => return Err(format!("model_http_{code}")),
            Err(ureq::Error::Timeout(_)) => return Err("model_timeout".to_string()),
            Err(_) => return Err("model_request_failed".to_string()),
        };
        let mode = tool_name_mode;
        let output = success_output(&endpoint.model, input.blocks.len(), value, &mode, choice);
        Ok(output)
    }
}

/// A no-op follow-up used when there is no prior tool call (first round). The
/// assistant/tool messages are harmless placeholders that providers ignore when
/// the conversation has no prior tool_calls to match.
fn empty_followup(_input: &LlmInput) -> LlmFollowUp {
    LlmFollowUp {
        provider_turn: ProviderToolTurn {
            endpoint: EndpointChoice::Primary,
            provider_tool_call_id: String::new(),
            wire_name: String::new(),
            canonical_operation: String::new(),
            arguments_json: String::new(),
        },
        result_content: String::new(),
    }
}

pub(crate) struct ModelEndpoint {
    base_url: String,
    api_key: String,
    model: String,
    pub(crate) tool_name_mode: ToolNameMode,
}

impl ModelEndpoint {
    fn new(base_url: String, api_key: String, model: String) -> Self {
        let normalized_model = normalize_model_name(&base_url, &model);
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model: normalized_model,
            tool_name_mode: ToolNameMode::Passthrough,
        }
    }
    fn with_indexed_tool_name(mut self) -> Self {
        self.tool_name_mode = ToolNameMode::IndexedMapping(ToolNameMap::new());
        self
    }
    fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty()
            && !self.api_key.trim().is_empty()
            && !self.model.trim().is_empty()
    }
    fn chat_completions_url(&self) -> String {
        if self.base_url.ends_with("/chat/completions") {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }
}

fn config_required_output(model: &str, context_blocks: usize) -> LlmOutput {
    LlmOutput {
        provider: "openai-compatible".to_string(),
        model: model.to_string(),
        content: "模型配置还没准备好：请先配置 AGENT_CORE_OPENAI_API_KEY 和 AGENT_CORE_MODEL。"
            .to_string(),
        journal_payload: json!({
            "provider": "openai-compatible",
            "model": empty_to_null(model),
            "context_blocks": context_blocks,
            "status": "needs_config",
            "error_category": "model_config_required",
        }),
        tool_call: ToolCallResult::Absent,
        provider_turn: None,
    }
}

fn request_failed_output(model: &str, context_blocks: usize, category: &str) -> LlmOutput {
    LlmOutput {
        provider: "openai-compatible".to_string(),
        model: model.to_string(),
        content: "模型调用失败了，我这边已经记录为一次失败的模型请求，稍后可以重试。".to_string(),
        journal_payload: json!({
            "provider": "openai-compatible",
            "model": model,
            "context_blocks": context_blocks,
            "status": "error",
            "error_category": category,
        }),
        tool_call: ToolCallResult::Absent,
        provider_turn: None,
    }
}

fn success_output(
    model: &str,
    context_blocks: usize,
    value: Value,
    mode: &ToolNameMode,
    choice: EndpointChoice,
) -> LlmOutput {
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string();
    let parsed = parse_tool_call(&value, mode, choice);
    let provider_turn = parsed.provider_turn;
    let tool_call = parsed.tool_call_result;
    let content = match &tool_call {
        ToolCallResult::Malformed(_) if content.is_empty() => {
            "The tool call could not be parsed. Please try again.".to_string()
        }
        ToolCallResult::Valid(_) if content.is_empty() => "正在调用工具查询信息…".to_string(),
        _ => content,
    };
    LlmOutput {
        provider: "openai-compatible".to_string(),
        model: value.get("model").and_then(Value::as_str).unwrap_or(model).to_string(),
        content,
        journal_payload: json!({
            "provider": "openai-compatible",
            "model": value.get("model").and_then(Value::as_str).unwrap_or(model),
            "context_blocks": context_blocks,
            "status": "ok",
            "usage": sanitize_usage(value.get("usage")),
            "tool_call": audit_tool_call(&tool_call),
        }),
        tool_call,
        provider_turn,
    }
}

/// Result of parsing a provider tool call: the kernel-authoritative ToolCall
/// plus the provider-side ProviderToolTurn (raw id, wire name, args JSON).
struct ParsedToolCall {
    tool_call_result: ToolCallResult,
    provider_turn: Option<ProviderToolTurn>,
}

const MAX_PROVIDER_ID_LEN: usize = 256;
const MAX_WIRE_NAME_LEN: usize = 128;
const MAX_ARGS_JSON_LEN: usize = 8192;

fn parse_tool_call(value: &Value, mode: &ToolNameMode, choice: EndpointChoice) -> ParsedToolCall {
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
                    tool_call_result: ToolCallResult::Malformed("unknown function name".to_string()),
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
                tool_call_result: ToolCallResult::Malformed(format!("arguments JSON parse error: {e}")),
                provider_turn: None,
            }
        }
    };
    let provider_turn = ProviderToolTurn {
        endpoint: choice,
        provider_tool_call_id: raw_id.to_string(),
        wire_name: raw_operation.to_string(),
        canonical_operation: operation.clone(),
        arguments_json: arguments_str.to_string(),
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

fn audit_tool_call(tool_call: &ToolCallResult) -> Value {
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

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn mark_fallback(mut output: LlmOutput, primary_model: &str, primary_error: &str) -> LlmOutput {
    if let Some(payload) = output.journal_payload.as_object_mut() {
        payload.insert(
            "fallback".to_string(),
            json!({
                "used": true,
                "primary_model": empty_to_null(primary_model),
                "primary_error_category": primary_error,
            }),
        );
    }
    output
}

fn serialize_system_context(blocks: &[ContextBlock]) -> String {
    blocks
        .iter()
        .filter(|block| !matches!(block.kind, crate::domain::ContextBlockKind::UserMessage))
        .map(|block| format!("## {:?}\n{}", block.kind, block.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn sanitize_usage(value: Option<&Value>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    json!({
        "prompt_tokens": value.get("prompt_tokens").and_then(Value::as_i64),
        "completion_tokens": value.get("completion_tokens").and_then(Value::as_i64),
        "total_tokens": value.get("total_tokens").and_then(Value::as_i64),
    })
}

fn empty_to_null(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

fn normalize_model_name(base_url: &str, model: &str) -> String {
    let trimmed = model.trim();
    if is_zai_endpoint(base_url) {
        return trimmed
            .strip_prefix("zai/")
            .or_else(|| trimmed.strip_prefix("z.ai/"))
            .unwrap_or(trimmed)
            .to_string();
    }
    trimmed.to_string()
}

fn is_zai_endpoint(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    lower.contains("z.ai") || lower.contains("bigmodel.cn")
}

#[cfg(test)]
mod tests;
