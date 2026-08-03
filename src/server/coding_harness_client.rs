//! Narrow localhost client for Gateway-approved controlled Harness calls.

use crate::domain::ApprovedInvocation;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const CODING_HARNESS_ADDR: &str = "127.0.0.1:7200";
const MAX_RESPONSE_BYTES: usize = 128 * 1024;

#[derive(Debug)]
pub enum CodingHarnessExecutionOutcome {
    Succeeded(Value),
    DefinitivelyRejected { error_code: String },
    OutcomeUnknown(anyhow::Error),
}

pub fn execute(approved: &ApprovedInvocation, timeout: Duration) -> CodingHarnessExecutionOutcome {
    match execute_inner(approved, timeout) {
        Ok(outcome) => outcome,
        Err(error) => CodingHarnessExecutionOutcome::OutcomeUnknown(error),
    }
}

fn execute_inner(
    approved: &ApprovedInvocation,
    timeout: Duration,
) -> Result<CodingHarnessExecutionOutcome> {
    let intent = approved.intent();
    if intent.operation != crate::domain::operation::external::TASK_SUBMIT {
        bail!("CODING_HARNESS_OPERATION_MISMATCH");
    }
    let mut arguments = intent.arguments.clone();
    if let Some(object) = arguments.as_object_mut() {
        object.insert(
            "invocation_intent_id".into(),
            Value::String(intent.invocation_id.0.clone()),
        );
    }
    let body = json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": intent.invocation_id.0,
        "operation": intent.operation,
        "arguments": arguments,
    });
    let bytes = serde_json::to_vec(&body)?;
    let control_token = std::env::var("AGENT_CORE_CODING_HARNESS_CONTROL_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("CODING_HARNESS_CONTROL_NOT_CONFIGURED"))?;
    let request = format!(
        "POST /execute HTTP/1.1\r\nHost: {CODING_HARNESS_ADDR}\r\nAuthorization: Bearer {control_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        bytes.len(),
        String::from_utf8_lossy(&bytes),
    );
    let mut stream = TcpStream::connect(CODING_HARNESS_ADDR)
        .map_err(|_| anyhow::anyhow!("CODING_HARNESS_CONNECT_FAILED"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(request.as_bytes())?;

    let mut raw = Vec::new();
    stream
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut raw)?;
    if raw.len() > MAX_RESPONSE_BYTES {
        bail!("CODING_HARNESS_RESPONSE_TOO_LARGE");
    }
    let response = String::from_utf8(raw).map_err(|_| anyhow::anyhow!("INVALID_UTF8"))?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status) {
        bail!("CODING_HARNESS_HTTP_ERROR");
    }
    let payload = response
        .split_once("\r\n\r\n")
        .map(|(_, payload)| payload)
        .ok_or_else(|| anyhow::anyhow!("CODING_HARNESS_MALFORMED_RESPONSE"))?;
    let value: Value = serde_json::from_str(payload)?;
    classify_response(value)
}

fn classify_response(value: Value) -> Result<CodingHarnessExecutionOutcome> {
    if value.get("protocol_version").and_then(Value::as_str) != Some("external-harness-v1") {
        bail!("CODING_HARNESS_PROTOCOL_MISMATCH");
    }
    match (
        value.get("outcome").and_then(Value::as_str),
        value.get("ok").and_then(Value::as_bool),
    ) {
        (Some("succeeded"), Some(true)) => Ok(CodingHarnessExecutionOutcome::Succeeded(
            value
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("CODING_HARNESS_RESULT_MISSING"))?,
        )),
        (Some("definitively_rejected"), Some(false)) => {
            let error_code = value
                .get("error_code")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("CODING_HARNESS_REJECTION_CODE_MISSING"))?;
            Ok(CodingHarnessExecutionOutcome::DefinitivelyRejected {
                error_code: error_code.to_string(),
            })
        }
        (Some("outcome_unknown"), _) => Ok(CodingHarnessExecutionOutcome::OutcomeUnknown(
            anyhow::anyhow!("CODING_HARNESS_REPORTED_OUTCOME_UNKNOWN"),
        )),
        _ => bail!("CODING_HARNESS_OUTCOME_MISSING_OR_INVALID"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_generic_rejection_is_definitive() {
        let rejected = classify_response(json!({
            "protocol_version": "external-harness-v1",
            "ok": false,
            "outcome": "definitively_rejected",
            "error_code": "ANY_BUSINESS_REASON"
        }))
        .unwrap();
        assert!(matches!(
            rejected,
            CodingHarnessExecutionOutcome::DefinitivelyRejected { error_code }
                if error_code == "ANY_BUSINESS_REASON"
        ));

        // An old or malformed Harness error has no proof that execution had
        // no successful effect, so the client refuses to call it definitive.
        assert!(classify_response(json!({
            "protocol_version": "external-harness-v1",
            "ok": false,
            "error_code": "ANY_BUSINESS_REASON"
        }))
        .is_err());
    }
}
