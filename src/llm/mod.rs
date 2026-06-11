use crate::domain::ContextBlock;
use anyhow::Result;
use serde_json::{json, Value};
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
    base_url: String,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl OpenAiCompatibleLlm {
    pub fn new(base_url: String, api_key: String, model: String, timeout_ms: u64) -> Self {
        let normalized_model = normalize_model_name(&base_url, &model);
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model: normalized_model,
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    fn endpoint(&self) -> String {
        if self.base_url.ends_with("/chat/completions") {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }
}

impl LlmClient for OpenAiCompatibleLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        if self.api_key.trim().is_empty() || self.model.trim().is_empty() {
            return Ok(config_required_output(&self.model, input.blocks.len()));
        }
        let body = json!({
            "model": self.model,
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
        });
        match self.request(body) {
            Ok(value) => Ok(success_output(&self.model, input.blocks.len(), value)),
            Err(error) => Ok(request_failed_output(
                &self.model,
                input.blocks.len(),
                error.as_str(),
            )),
        }
    }
}

impl OpenAiCompatibleLlm {
    fn request(&self, body: Value) -> std::result::Result<Value, String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .build()
            .new_agent();
        let response = agent
            .post(&self.endpoint())
            .header("authorization", &format!("Bearer {}", self.api_key))
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
    }
}

fn success_output(model: &str, context_blocks: usize, value: Value) -> LlmOutput {
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("模型没有返回文本内容。")
        .to_string();
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
        }),
    }
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
