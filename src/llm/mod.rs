use crate::domain::ContextBlock;
use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub trait LlmClient {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput>;
}

pub struct LlmInput {
    pub blocks: Vec<ContextBlock>,
    pub user_text: String,
}

pub struct LlmOutput {
    pub provider: String,
    pub model: String,
    pub content: String,
    pub journal_payload: serde_json::Value,
    /// Parsed tool call from the provider response. Three-way state:
    /// - `Absent` — no tool call in the response (text-only flow).
    /// - `Valid(tc)` — a parseable tool call to execute.
    /// - `Malformed(reason)` — the provider sent tool_calls but arguments
    ///   were unparseable; never executes, produces a stable ToolResult.
    pub tool_call: ToolCallResult,
}

/// The result of parsing a provider tool-call response.
#[derive(Debug, Clone)]
pub enum ToolCallResult {
    /// No tool call in the provider response (text-only).
    Absent,
    /// A valid, parseable tool call.
    Valid(ToolCall),
    /// Provider sent tool_calls but arguments were malformed. The model
    /// receives a ToolResult with the sanitized error and can recover.
    Malformed(String),
}

impl ToolCallResult {
    pub fn is_absent(&self) -> bool {
        matches!(self, ToolCallResult::Absent)
    }
}

/// Deterministic bounded hash of a provider tool-call ID. The provider value
/// is untrusted/unbounded; we never write it raw into Journal, idempotency keys,
/// errors, or model-visible text.
pub fn tool_call_id_hash(provider_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(provider_id.as_bytes());
    hex::encode(hasher.finalize())
}

/// A structured tool call the model wishes to execute (Phase 2 tool-call MVP).
/// `operation` must be a catalogued `ReadOnly` operation or the call is
/// rejected before any adapter runs.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Deterministic bounded digest of the provider-assigned id. Never the raw
    /// provider value — the raw value is untrusted and never written to Journal,
    /// idempotency keys, errors, or model-visible text.
    pub id: String,
    /// Catalogued operation name (e.g. `time.now`).
    pub operation: String,
    /// JSON arguments for the operation.
    pub arguments: serde_json::Value,
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
    primary: ModelEndpoint,
    fallback: Option<ModelEndpoint>,
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
}

impl LlmClient for OpenAiCompatibleLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        if !self.primary.is_configured() {
            return match self.try_fallback(&input, "model_config_required") {
                Some(output) => Ok(output),
                None => Ok(config_required_output(
                    &self.primary.model,
                    input.blocks.len(),
                )),
            };
        }
        match self.request_endpoint(&self.primary, &input) {
            Ok(value) => Ok(success_output(
                &self.primary.model,
                input.blocks.len(),
                value,
            )),
            Err(error) => {
                if let Some(output) = self.try_fallback(&input, error.as_str()) {
                    return Ok(output);
                }
                Ok(request_failed_output(
                    &self.primary.model,
                    input.blocks.len(),
                    error.as_str(),
                ))
            }
        }
    }
}

impl OpenAiCompatibleLlm {
    fn try_fallback(&self, input: &LlmInput, primary_error: &str) -> Option<LlmOutput> {
        let fallback = self.fallback.as_ref()?;
        let output = match self.request_endpoint(fallback, input) {
            Ok(value) => success_output(&fallback.model, input.blocks.len(), value),
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
    ) -> std::result::Result<Value, String> {
        let body = json!({
            "model": endpoint.model,
            "messages": [
                {
                    "role": "system",
                    "content": serialize_system_context(&input.blocks),
                },
                {
                    "role": "user",
                    "content": input.user_text,
                },
            ],
            "temperature": 0.2,
            "tools": read_only_tool_schemas(),
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
        match response {
            Ok(mut response) => response
                .body_mut()
                .read_json::<Value>()
                .map_err(|_| "model_response_parse_failed".to_string()),
            Err(ureq::Error::StatusCode(code)) => Err(format!("model_http_{code}")),
            Err(ureq::Error::Timeout(_)) => Err("model_timeout".to_string()),
            Err(_) => Err("model_request_failed".to_string()),
        }
    }
}

struct ModelEndpoint {
    base_url: String,
    api_key: String,
    model: String,
}

impl ModelEndpoint {
    fn new(base_url: String, api_key: String, model: String) -> Self {
        let normalized_model = normalize_model_name(&base_url, &model);
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model: normalized_model,
        }
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

fn success_output(model: &str, context_blocks: usize, value: Value) -> LlmOutput {
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string();
    // Parse tool_calls[0] from the OpenAI-compatible response.
    let tool_call = parse_tool_call(&value);
    // When the provider returns a malformed tool call with no text content,
    // ensure a non-empty user-safe fallback so the reply is never silently empty.
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
            "tool_call": match &tool_call {
                ToolCallResult::Valid(tc) => json!({
                    "operation": tc.operation,
                    "id": tc.id,
                }),
                ToolCallResult::Malformed(reason) => json!({
                    "malformed": reason,
                }),
                ToolCallResult::Absent => Value::Null,
            },
        }),
        tool_call,
    }
}

/// Parse `choices[0].message.tool_calls[0]` from an OpenAI-compatible response.
/// Returns `ToolCallResult` — three-way state distinguishing absent, valid,
/// and malformed tool calls. Never returns raw provider IDs or arguments.
///
/// Malformed `function.arguments` (invalid JSON, non-object JSON) is rejected
/// as `Malformed(reason)` — the model receives a ToolResult and can recover.
fn parse_tool_call(value: &Value) -> ToolCallResult {
    let tool_call_json = match value.pointer("/choices/0/message/tool_calls/0") {
        Some(v) if !v.is_null() => v,
        _ => return ToolCallResult::Absent,
    };
    let function = match tool_call_json.get("function") {
        Some(f) => f,
        None => return ToolCallResult::Malformed("missing function block".to_string()),
    };
    let raw_id = tool_call_json
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = tool_call_id_hash(raw_id);
    let operation = match function.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => {
            return ToolCallResult::Malformed("missing function name".to_string());
        }
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

/// Build the OpenAI-compatible `tools` array for the request — only ReadOnly
/// operations are exposed to the model, with strict parameter schemas. Every
/// schema has `additionalProperties: false` so the model cannot inject extra
/// fields that bypass validation.
fn read_only_tool_schemas() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "time.now",
                "description": "Return the current kernel wall-clock time (ISO-8601 + epoch ms).",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "session.recall_recent",
                "description": "Recall recent messages from the current session (read-only, current session only).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Max messages to recall (default 5)." },
                        "query": { "type": "string", "description": "Optional case-insensitive substring filter." }
                    },
                    "required": [],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "system.status",
                "description": "Return system health and projection summary (aggregate counts only, no secrets).",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }
            }
        }
    ])
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
