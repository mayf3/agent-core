//! Narrow localhost client for Gateway-approved controlled Harness calls.

use crate::domain::ApprovedInvocation;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const CODING_HARNESS_ADDR: &str = "127.0.0.1:7200";
const MAX_RESPONSE_BYTES: usize = 128 * 1024;

pub fn execute(approved: &ApprovedInvocation, timeout: Duration) -> Result<Value> {
    let intent = approved.intent();
    if intent.operation != crate::domain::operation::external::TASK_SUBMIT {
        bail!("CODING_HARNESS_OPERATION_MISMATCH");
    }
    let body = json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": intent.invocation_id.0,
        "operation": intent.operation,
        "arguments": intent.arguments,
    });
    let bytes = serde_json::to_vec(&body)?;
    let request = format!(
        "POST /execute HTTP/1.1\r\nHost: {CODING_HARNESS_ADDR}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
    if value.get("protocol_version").and_then(Value::as_str) != Some("external-harness-v1")
        || value.get("ok").and_then(Value::as_bool) != Some(true)
    {
        let harness_code = value
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        bail!("CODING_HARNESS_SUBMIT_FAILED:{}", harness_code);
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("CODING_HARNESS_RESULT_MISSING"))
}
