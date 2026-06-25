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
}
pub struct LlmOutput {
    pub provider: String,
    pub model: String,
    pub content: String,
    pub journal_payload: serde_json::Value,

    pub tool_call: ToolCallResult,
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

#[derive(Debug, Clone)]
pub struct ToolCall {

    pub id: String,

    pub operation: String,

    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) enum ToolNameMode {

    Passthrough,

    IndexedMapping(ToolNameMap),
}

pub(crate) type ToolNameMap = HashMap<String, String>;
pub(crate) type ToolCallRawMap = std::collections::HashMap<String, ToolCallRawData>;

#[derive(Debug, Clone)]
pub(crate) struct ToolCallRawData {
    pub provider_id: String,
    pub wire_name: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone)]
pub struct ProviderToolTurn {
    pub provider_tool_call_id: String,
    pub wire_name: String,
    pub canonical_operation: String,
    pub arguments_json: String,
    pub result_content: String,
} #[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointChoice {
    Primary, Fallback,
}
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
        })
    }
}
pub struct OpenAiCompatibleLlm {
    pub(crate) primary: ModelEndpoint,
    pub(crate) fallback: Option<ModelEndpoint>,
    timeout: Duration,
    pub(crate) pending_transcript: std::cell::RefCell<LlmFollowUp>,
    pub(crate) pending_raw_map: std::cell::RefCell<ToolCallRawMap>,
} #[derive(Debug, Clone, Default)]
pub struct LlmFollowUp {
    pub transcript: Vec<ProviderToolTurn>,
    pub endpoint: Option<EndpointChoice>,
}
impl OpenAiCompatibleLlm {
    pub fn new(base_url: String, api_key: String, model: String, timeout_ms: u64) -> Self {
        Self {
            primary: ModelEndpoint::new(base_url, api_key, model),
            fallback: None,
            timeout: Duration::from_millis(timeout_ms),
            pending_transcript: std::cell::RefCell::new(LlmFollowUp::default()),
            pending_raw_map: std::cell::RefCell::new(ToolCallRawMap::new()),
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
std::thread_local! {
    static LAST_RAW_TOOL_CALL: std::cell::RefCell<Option<ToolCallRawData>> = std::cell::RefCell::new(None);
}
impl LlmClient for OpenAiCompatibleLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        let follow_up = self.pending_transcript.take();
        let chosen = follow_up.endpoint;
        if chosen == Some(EndpointChoice::Fallback) {
            if let Some(fb) = &self.fallback {
                return match self.request_endpoint(fb, &input, &follow_up.transcript) {
                    Ok((v,m)) => Ok(Self::cc(success_output(&fb.model, input.blocks.len(), v, &m), &self.pending_raw_map)),
                    Err(e) => Ok(request_failed_output(&fb.model, input.blocks.len(), e.as_str())),
                };
            }
        }
        if !self.primary.is_configured() {
            return match self.try_fallback(&input, "model_config_required") {
                Some(o) => Ok(o),
                None => Ok(config_required_output(&self.primary.model, input.blocks.len())),
            };
        }
        match self.request_endpoint(&self.primary, &input, &follow_up.transcript) {
            Ok((value, mode)) => Ok(Self::cc(success_output(&self.primary.model, input.blocks.len(), value, &mode), &self.pending_raw_map)),
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
    fn cc(output: LlmOutput, map: &std::cell::RefCell<ToolCallRawMap>) -> LlmOutput {
        LAST_RAW_TOOL_CALL.with(|cell| {
            if let Some(raw) = cell.borrow_mut().take() {
                let mut m = map.borrow_mut();
                if let ToolCallResult::Valid(ref tc) = output.tool_call {
                    m.insert(tc.id.clone(), raw);
                }
            }
        });
        output
    }
    fn try_fallback(&self, input: &LlmInput, primary_error: &str) -> Option<LlmOutput> {
        let fallback = self.fallback.as_ref()?;
        let output = match self.request_endpoint(fallback, input, &[]) {
            Ok((value, mode)) => success_output(&fallback.model, input.blocks.len(), value, &mode),
            Err(error) => {
                request_failed_output(&fallback.model, input.blocks.len(), error.as_str())
            }
        };
        Some(mark_fallback(output, &self.primary.model, primary_error))
    }
    fn request_endpoint(
        &self,
        endpoint: &ModelEndpoint,
        input: &LlmInput,
        transcript: &[ProviderToolTurn],
    ) -> std::result::Result<(Value, ToolNameMode), String> {
        let transcript = transcript;
        let mut tools =
            crate::domain::operation::provider_tools_for_grants(&input.granted_operations);
        // Build per-request mapping if endpoint uses IndexedMapping.
        let tool_name_mode = match &endpoint.tool_name_mode {
            ToolNameMode::Passthrough => ToolNameMode::Passthrough,
            ToolNameMode::IndexedMapping(_) => {
                let mut map = ToolNameMap::new();
                for (idx, tool) in tools.iter_mut().enumerate() {
                    if let Some(name) = tool.pointer("/function/name").and_then(Value::as_str) {
                        let safe = format!("fn_{}", idx);
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
        for turn in transcript {
            messages.push(json!({"role":"assistant","tool_calls":[{"id":turn.provider_tool_call_id,"type":"function","function":{"name":turn.wire_name,"arguments":turn.arguments_json}}]}));
            messages.push(json!({"role":"tool","tool_call_id":turn.provider_tool_call_id,"content":turn.result_content}));
        }
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
        let response_value = match response {
            Ok(mut response) => response
                .body_mut()
                .read_json::<Value>()
                .map_err(|_| "model_response_parse_failed".to_string()),
            Err(ureq::Error::StatusCode(code)) => Err(format!("model_http_{code}")),
            Err(ureq::Error::Timeout(_)) => Err("model_timeout".to_string()),
            Err(_) => Err("model_request_failed".to_string()),
        };
        response_value.map(|v| (v, tool_name_mode))
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
    }
}
fn success_output(
    model: &str,
    context_blocks: usize,
    value: Value,
    mode: &ToolNameMode,
) -> LlmOutput {
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string();
    // Parse tool_calls[0] from the OpenAI-compatible response.
    let tool_call = parse_tool_call(&value, mode);
    // User-safe fallback for malformed tool calls with empty content.
    let content = match &tool_call {
        ToolCallResult::Malformed(_) if content.is_empty() => {
            "The tool call could not be parsed. Please try again.".to_string()
        }
        ToolCallResult::Valid(_) if content.is_empty() => "正在调用工具查询信息…".to_string(),
        _ => content,
    };
    LlmOutput {
        provider: "openai-compatible".to_string(),
        model: model.to_string(),
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
    }
}
fn parse_tool_call(value: &Value, mode: &ToolNameMode) -> ToolCallResult {
    let tool_call_json = match value.pointer("/choices/0/message/tool_calls/0") {
        Some(v) if !v.is_null() => v,
        _ => return ToolCallResult::Absent,
    };
    let function = match tool_call_json.get("function") {
        Some(f) => f,
        None => return ToolCallResult::Malformed("missing function block".to_string()),
    };
    // A missing/empty id is malformed (never synthesize "unknown"). The raw id
    // is hashed once here at the DTO boundary; downstream treats it as opaque.
    let raw_id = match tool_call_json.get("id").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return ToolCallResult::Malformed("missing tool_call id".to_string()),
    };
    let id = tool_call_id_hash(raw_id);
    let raw_operation = match function.get("name").and_then(Value::as_str) {
        Some(n) if !n.trim().is_empty() => n.to_string(),
        _ => return ToolCallResult::Malformed("missing function name".to_string()),
    };
    // Resolve provider-safe name → canonical. IndexedMapping: per-request map,
    // unknowns are Malformed. Passthrough: use provider name as-is.
    let operation = match mode {
        ToolNameMode::Passthrough => raw_operation,
        ToolNameMode::IndexedMapping(map) => match map.get(&raw_operation) {
            Some(canonical) => canonical.clone(),
            None => return ToolCallResult::Malformed("unknown function name".to_string()),
        },
    };
    let arguments_str = function.get("arguments").and_then(Value::as_str);
    let arguments_val = match arguments_str {
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) if v.is_object() => v,
            Ok(v) => {
                return ToolCallResult::Malformed(format!(
                    "arguments must be a JSON object, got {}",
                    type_name(&v)
                ));
            }
            Err(e) => {
                return ToolCallResult::Malformed(format!("arguments JSON parse error: {}", e));
            }
        },
        None => {
            return ToolCallResult::Malformed("missing arguments".to_string());
        }
    };
    ToolCallResult::Valid(ToolCall {
        id,
        operation,
        arguments: arguments_val,
    })
}
fn audit_tool_call(tool_call: &ToolCallResult) -> Value {
    match tool_call {
        ToolCallResult::Valid(call) => json!({
            "operation": crate::domain::operation::lookup(&call.operation)
                .map(|spec| spec.name)
                .unwrap_or("unknown_operation"),
            "id": call.id,
        }),
        ToolCallResult::Malformed(_) => json!({
            "malformed": "malformed_tool_call",
        }),
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