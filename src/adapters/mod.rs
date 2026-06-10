use crate::domain::{ApprovedInvocation, Receipt, ReceiptStatus};
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub trait InvocationAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt>;
}

pub struct HttpConnectorAdapter {
    execute_url: String,
    ipc_token: String,
    timeout: Duration,
}

impl HttpConnectorAdapter {
    pub fn new(execute_url: String, ipc_token: String) -> Self {
        Self {
            execute_url,
            ipc_token,
            timeout: Duration::from_secs(10),
        }
    }
}

impl InvocationAdapter for HttpConnectorAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
        let body = json!({
            "protocol_version": "v1",
            "invocation_id": invocation.intent().invocation_id.0,
            "decision_id": invocation.decision_id,
            "operation": invocation.intent().operation,
            "arguments": invocation.intent().arguments,
            "idempotency_key": invocation.intent().idempotency_key,
        });
        let response = post_json(&self.execute_url, &self.ipc_token, &body, self.timeout)?;
        let receipt = response
            .get("receipt")
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            external_ref: receipt
                .get("message_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            output: receipt,
            occurred_at: Utc::now(),
        })
    }
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

fn post_json(url: &str, token: &str, body: &Value, timeout: Duration) -> Result<Value> {
    let endpoint = Endpoint::parse(url)?;
    let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let payload = serde_json::to_string(body)?;
    let auth = if token.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {token}\r\n")
    };
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        endpoint.path,
        endpoint.host,
        auth,
        payload.len(),
        payload
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let Some((head, response_body)) = response.split_once("\r\n\r\n") else {
        bail!("connector returned malformed HTTP response");
    };
    if !head.starts_with("HTTP/1.1 2") && !head.starts_with("HTTP/1.0 2") {
        bail!("connector execute failed");
    }
    Ok(serde_json::from_str(response_body.trim()).unwrap_or_else(|_| json!({})))
}

struct Endpoint {
    host: String,
    port: u16,
    path: String,
}

impl Endpoint {
    fn parse(url: &str) -> Result<Self> {
        let Some(rest) = url.strip_prefix("http://") else {
            bail!("only http connector URLs are supported in Phase 0");
        };
        let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = host_port.split_once(':').unwrap_or((host_port, "80"));
        if host != "127.0.0.1" && host != "localhost" {
            bail!("connector URL must bind to localhost");
        }
        Ok(Self {
            host: host.to_string(),
            port: port.parse()?,
            path: format!("/{}", path),
        })
    }
}
