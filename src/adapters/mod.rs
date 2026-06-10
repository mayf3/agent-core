use crate::domain::{ApprovedInvocation, Receipt, ReceiptStatus};
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

pub trait InvocationAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt>;
}

pub struct StdoutAdapter;

impl InvocationAdapter for StdoutAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
        let output = string_arg(&invocation.intent().arguments, "text")?;
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            external_ref: Some("stdout".to_string()),
            output: json!({ "text": output }),
            occurred_at: Utc::now(),
        })
    }
}

fn string_arg(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing string argument: {key}"))
}
