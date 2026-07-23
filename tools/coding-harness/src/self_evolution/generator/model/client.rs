use super::ModelConfig;
use crate::self_evolution::generator::GenerationError;
use serde_json::{json, Value};

pub(super) fn complete(
    config: &ModelConfig,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, GenerationError> {
    let body = json!({
        "model": config.model,
        "temperature": 0.1,
        "max_tokens": 12_000,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt}
        ]
    });
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(config.timeout))
        .build()
        .new_agent();
    let response = agent
        .post(&config.endpoint)
        .header("authorization", &format!("Bearer {}", config.api_key))
        .header("content-type", "application/json")
        .send_json(body);
    let mut response = match response {
        Ok(response) => response,
        Err(_) => return Err(GenerationError::new("GENERATOR_MODEL_UNAVAILABLE")),
    };
    let value: Value = response
        .body_mut()
        .read_json()
        .map_err(|_| GenerationError::new("GENERATOR_MODEL_RESPONSE_INVALID"))?;
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| GenerationError::new("GENERATOR_MODEL_RESPONSE_INVALID"))?;
    if value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("length")
    {
        return Err(GenerationError::new("GENERATOR_MODEL_OUTPUT_TRUNCATED"));
    }
    Ok(strip_markdown_fence(content))
}

pub(super) fn strip_markdown_fence(content: &str) -> String {
    let trimmed = content.trim();
    let Some(fence) = trimmed.find("```") else {
        return trimmed.to_string();
    };
    let fenced = &trimmed[fence..];
    let body_start = fenced.find('\n').map(|index| index + 1).unwrap_or(3);
    let body = &fenced[body_start..];
    let body_end = body.find("\n```").unwrap_or(body.len());
    body[..body_end].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_markdown_fence;

    #[test]
    fn strips_optional_markdown_fence() {
        assert_eq!(strip_markdown_fence("```rust\nfn x() {}\n```"), "fn x() {}");
        assert_eq!(strip_markdown_fence("fn x() {}"), "fn x() {}");
    }
}
