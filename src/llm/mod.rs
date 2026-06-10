use crate::domain::ContextBlock;
use anyhow::Result;
use serde_json::json;

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
