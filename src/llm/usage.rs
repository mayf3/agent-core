use super::LlmOutput;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost: Option<f64>,
    pub provider_usage_extensions: Value,
}

impl LlmOutput {
    pub fn normalized_usage(&self) -> ModelUsage {
        let usage = self.journal_payload.get("usage").unwrap_or(&Value::Null);
        ModelUsage {
            input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
            cached_input_tokens: usage.get("cached_input_tokens").and_then(Value::as_u64),
            output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
            reasoning_tokens: usage.get("reasoning_tokens").and_then(Value::as_u64),
            total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
            estimated_cost: usage.get("estimated_cost").and_then(Value::as_f64),
            provider_usage_extensions: usage
                .get("provider_usage_extensions")
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| json!({})),
        }
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.journal_payload
            .get("finish_reason")
            .and_then(Value::as_str)
    }

    pub fn failure_category(&self) -> Option<&str> {
        match self.journal_payload.get("status").and_then(Value::as_str) {
            Some("error" | "needs_config") => self
                .journal_payload
                .get("error_category")
                .and_then(Value::as_str)
                .or(Some("model_request_failed")),
            _ => None,
        }
    }
}

pub(super) fn sanitize_usage(value: Option<&Value>) -> Value {
    let Some(value) = value.filter(|value| value.is_object()) else {
        return json!({
            "input_tokens": null,
            "cached_input_tokens": null,
            "output_tokens": null,
            "reasoning_tokens": null,
            "total_tokens": null,
            "estimated_cost": null,
            "provider_usage_extensions": {},
        });
    };
    let input_tokens = usage_counter(value, &["input_tokens", "prompt_tokens"]);
    let cached_input_tokens = usage_counter_at_paths(
        value,
        &[
            "/cached_input_tokens",
            "/input_tokens_details/cached_tokens",
            "/prompt_tokens_details/cached_tokens",
        ],
    );
    let output_tokens = usage_counter(value, &["output_tokens", "completion_tokens"]);
    let reasoning_tokens = usage_counter_at_paths(
        value,
        &[
            "/reasoning_tokens",
            "/output_tokens_details/reasoning_tokens",
            "/completion_tokens_details/reasoning_tokens",
        ],
    );
    let total_tokens = usage_counter(value, &["total_tokens"])
        .or_else(|| Some(input_tokens?.checked_add(output_tokens?)?));
    let estimated_cost = value
        .get("estimated_cost")
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0);
    json!({
        "input_tokens": input_tokens,
        "cached_input_tokens": cached_input_tokens,
        "output_tokens": output_tokens,
        "reasoning_tokens": reasoning_tokens,
        "total_tokens": total_tokens,
        "estimated_cost": estimated_cost,
        "provider_usage_extensions": sanitize_usage_extensions(value),
    })
}

fn usage_counter(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn usage_counter_at_paths(value: &Value, paths: &[&str]) -> Option<u64> {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(Value::as_u64))
}

fn sanitize_usage_extensions(value: &Value) -> Value {
    let mut extensions = serde_json::Map::new();
    let Some(source) = value.as_object() else {
        return Value::Object(extensions);
    };
    let standard = [
        "input_tokens",
        "prompt_tokens",
        "cached_input_tokens",
        "input_tokens_details",
        "prompt_tokens_details",
        "output_tokens",
        "completion_tokens",
        "reasoning_tokens",
        "output_tokens_details",
        "completion_tokens_details",
        "total_tokens",
        "estimated_cost",
    ];
    for (key, item) in source {
        if standard.contains(&key.as_str()) || sensitive_usage_key(key) {
            continue;
        }
        if let Some(safe) = numeric_extension(item, 0) {
            extensions.insert(key.clone(), safe);
        }
    }
    Value::Object(extensions)
}

fn numeric_extension(value: &Value, depth: usize) -> Option<Value> {
    if depth > 4 {
        return None;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::Array(values) if values.len() <= 32 => Some(Value::Array(
            values
                .iter()
                .filter_map(|value| numeric_extension(value, depth + 1))
                .collect(),
        )),
        Value::Object(values) if values.len() <= 64 => {
            let mut result = serde_json::Map::new();
            for (key, value) in values {
                if !sensitive_usage_key(key) {
                    if let Some(safe) = numeric_extension(value, depth + 1) {
                        result.insert(key.clone(), safe);
                    }
                }
            }
            Some(Value::Object(result))
        }
        _ => None,
    }
}

fn sensitive_usage_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("secret")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("password")
        || key == "authorization"
        || key == "access_token"
        || key == "refresh_token"
}
