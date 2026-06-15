use crate::domain::{ApprovedInvocation, Receipt, ReceiptStatus};
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
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
    // Bind the connect phase to the same timeout budget as read/write. The
    // dispatcher thread joins on the in-flight adapter call during shutdown,
    // so an unbounded connect would drag shutdown. Errors are sanitized to a
    // category string — the raw transport error is never surfaced so it
    // cannot leak host/port/credential detail into logs or the journal.
    let mut stream = connect_with_timeout(&endpoint, timeout)?;
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

/// Open a TCP connection to the endpoint with a bounded connect timeout.
///
/// `TcpStream::connect_timeout` requires a resolved `SocketAddr`, so the
/// host:port is resolved first. All failures are mapped to a sanitized
/// category string so the raw transport error (which may echo the host, port,
/// or OS-level detail) is never written to logs or the journal:
///
/// - timeout (resolve or connect) → `"adapter connect timeout"` — contains
///   "timeout" so `DispatchErrorCategory::from_error` classifies it as
///   `AdapterTimeout`.
/// - any other failure → `"adapter connect failed"` — contains "adapter" so
///   it classifies as `AdapterFailed`.
fn connect_with_timeout(endpoint: &Endpoint, timeout: Duration) -> Result<TcpStream> {
    let address = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(|_| anyhow!("adapter connect failed"))?
        .next()
        .ok_or_else(|| anyhow!("adapter connect failed"))?;
    match TcpStream::connect_timeout(&address, timeout) {
        Ok(stream) => Ok(stream),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            Err(anyhow!("adapter connect timeout"))
        }
        Err(_) => Err(anyhow!("adapter connect failed")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DispatchErrorCategory;
    use std::net::TcpListener;

    /// Build an `Endpoint` pointing at an idle localhost port (a port that was
    /// just released) so `connect_with_timeout` hits connection-refused.
    fn idle_endpoint() -> Endpoint {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind idle port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        Endpoint {
            host: "127.0.0.1".to_string(),
            port,
            path: "/execute".to_string(),
        }
    }

    #[test]
    fn connect_refused_is_sanitized_to_category_string() {
        let endpoint = idle_endpoint();
        let error = connect_with_timeout(&endpoint, Duration::from_secs(1))
            .expect_err("expected connect failure on idle port");
        let message = error.to_string();
        assert_eq!(message, "adapter connect failed");
        // The raw OS error must never be surfaced — this is the sanitization
        // guarantee from HANDOVER §4.3.
        assert!(
            !message.contains("os error"),
            "raw OS error leaked into message: {message}"
        );
        assert!(
            !message.contains("refused"),
            "raw 'refused' detail leaked into message: {message}"
        );
    }

    #[test]
    fn connect_failed_classifies_as_adapter_failed() {
        let endpoint = idle_endpoint();
        let error = connect_with_timeout(&endpoint, Duration::from_secs(1))
            .expect_err("expected connect failure");
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::AdapterFailed
        );
    }

    #[test]
    fn connect_timeout_message_classifies_as_adapter_timeout() {
        // We do not trigger a real timeout (slow/flaky); instead we verify the
        // sanitized timeout string routes through the existing classifier the
        // same way a real connect_timeout TimedOut would.
        let error = anyhow::Error::msg("adapter connect timeout");
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::AdapterTimeout
        );
        // And the sanitized failed string must NOT classify as timeout.
        let failed = anyhow::Error::msg("adapter connect failed");
        assert_ne!(
            DispatchErrorCategory::from_error(&failed),
            DispatchErrorCategory::AdapterTimeout
        );
    }

    #[test]
    fn adapter_execute_surfaces_sanitized_connect_error() {
        // End-to-end through the public adapter API: a connect failure must
        // surface the sanitized category string, not the OS detail.
        let endpoint = idle_endpoint();
        let adapter = HttpConnectorAdapter::new(
            format!("http://127.0.0.1:{}/execute", endpoint.port),
            String::new(),
        );
        let invocation = ApprovedInvocation::new(
            crate::domain::InvocationIntent {
                invocation_id: crate::domain::InvocationId("test:connect".to_string()),
                run_id: crate::domain::RunId::new(),
                operation: "stdout.send_text".to_string(),
                arguments: json!({ "text": "hi" }),
                idempotency_key: Some("test:connect".to_string()),
            },
            "decision:test".to_string(),
        );
        let error = adapter
            .execute(&invocation)
            .expect_err("expected connect failure");
        assert_eq!(error.to_string(), "adapter connect failed");
        assert!(!error.to_string().contains("os error"));
    }
}
