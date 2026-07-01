use crate::domain::{AdapterError, ApprovedInvocation, Receipt, ReceiptStatus};
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub mod external_harness;

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
        // M2a: read the connector's reported receipt status instead of
        // assuming every 2xx response means Succeeded. The connector execute
        // response may carry `receipt.status` of "Succeeded" / "Failed" /
        // "Unknown"; a missing or unrecognized status falls back to Succeeded
        // to preserve the prior behavior for connectors that never set it.
        let status = receipt
            .get("status")
            .and_then(Value::as_str)
            .and_then(|s| match s {
                "Succeeded" => Some(ReceiptStatus::Succeeded),
                "Failed" => Some(ReceiptStatus::Failed),
                "Unknown" => Some(ReceiptStatus::Unknown),
                _ => None,
            })
            .unwrap_or(ReceiptStatus::Succeeded);
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status,
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

fn string_arg(value: &Value, key: &str) -> Result<String, AdapterError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AdapterError::InvalidArgument(format!("missing string argument: {key}")))
}

fn post_json(
    url: &str,
    token: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value, AdapterError> {
    let endpoint = Endpoint::parse(url).map_err(|e| AdapterError::Transport(e.to_string()))?;
    // Bind the connect phase to the same timeout budget as read/write. The
    // dispatcher thread joins on the in-flight adapter call during shutdown,
    // so an unbounded connect would drag shutdown.
    let mut stream = connect_with_timeout(&endpoint, timeout)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| AdapterError::Transport(e.to_string()))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| AdapterError::Transport(e.to_string()))?;
    let payload =
        serde_json::to_string(body).map_err(|e| AdapterError::Transport(e.to_string()))?;
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
    stream
        .write_all(request.as_bytes())
        .map_err(|e| io_error_to_adapter(e, timeout))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| io_error_to_adapter(e, timeout))?;
    let Some((head, response_body)) = response.split_once("\r\n\r\n") else {
        return Err(AdapterError::MalformedResponse);
    };
    if !head.starts_with("HTTP/1.1 2") && !head.starts_with("HTTP/1.0 2") {
        return Err(AdapterError::ExecuteFailed);
    }
    Ok(serde_json::from_str(response_body.trim()).unwrap_or_else(|_| json!({})))
}

/// Map an IO error from a read/write on the adapter socket to the right
/// `AdapterError` variant: timeouts → `Timeout`, everything else → `Transport`.
fn io_error_to_adapter(error: std::io::Error, _timeout: Duration) -> AdapterError {
    if error.kind() == std::io::ErrorKind::TimedOut
        || error.kind() == std::io::ErrorKind::WouldBlock
    {
        AdapterError::Timeout
    } else {
        AdapterError::Transport(error.to_string())
    }
}

/// Open a TCP connection to the endpoint with a bounded connect timeout.
///
/// `TcpStream::connect_timeout` requires a resolved `SocketAddr`, so the
/// host:port is resolved first. Failures map to typed `AdapterError` variants:
/// `TimedOut` → `Timeout`; anything else (refused, DNS, etc.) → `Transport`.
/// The raw transport error message is carried in the `Transport` variant but
/// is only surfaced via `DispatchErrorCategory` as a category string, never as
/// the raw text in logs/journal.
fn connect_with_timeout(endpoint: &Endpoint, timeout: Duration) -> Result<TcpStream, AdapterError> {
    let address = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(|e| AdapterError::Transport(e.to_string()))?
        .next()
        .ok_or_else(|| AdapterError::Transport("no socket address resolved".to_string()))?;
    match TcpStream::connect_timeout(&address, timeout) {
        Ok(stream) => Ok(stream),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => Err(AdapterError::Timeout),
        Err(error) => Err(AdapterError::Transport(error.to_string())),
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
    fn connect_refused_is_a_typed_transport_error() {
        let endpoint = idle_endpoint();
        let error = connect_with_timeout(&endpoint, Duration::from_secs(1))
            .expect_err("expected connect failure on idle port");
        // A refused connection is a Transport variant (not Timeout). The raw
        // OS message lives inside the variant but is never surfaced as a
        // category string — see the classification tests below.
        assert!(matches!(error, AdapterError::Transport(_)));
        assert!(!matches!(error, AdapterError::Timeout));
    }

    #[test]
    fn connect_failed_classifies_as_unknown_transport() {
        let endpoint = idle_endpoint();
        let error = connect_with_timeout(&endpoint, Duration::from_secs(1))
            .expect_err("expected connect failure");
        // The typed error is wrapped in anyhow at the trait boundary; classify
        // via from_error which downcasts to AdapterError.
        let anyhow_error = anyhow::Error::new(error);
        assert_eq!(
            DispatchErrorCategory::from_error(&anyhow_error),
            DispatchErrorCategory::UnknownTransportError
        );
    }

    #[test]
    fn timeout_variant_classifies_as_adapter_timeout() {
        let error = anyhow::Error::new(AdapterError::Timeout);
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::AdapterTimeout
        );
        // A Transport variant must NOT classify as timeout.
        let transport = anyhow::Error::new(AdapterError::Transport("refused".to_string()));
        assert_ne!(
            DispatchErrorCategory::from_error(&transport),
            DispatchErrorCategory::AdapterTimeout
        );
    }

    #[test]
    fn execute_failed_variant_classifies_as_connector_execute_failed() {
        let error = anyhow::Error::new(AdapterError::ExecuteFailed);
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::ConnectorExecuteFailed
        );
    }

    #[test]
    fn malformed_response_classifies_as_adapter_failed() {
        let error = anyhow::Error::new(AdapterError::MalformedResponse);
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::AdapterFailed
        );
    }

    #[test]
    fn invalid_argument_classifies_as_invalid_approved_invocation() {
        let error = anyhow::Error::new(AdapterError::InvalidArgument("missing text".to_string()));
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::InvalidApprovedInvocation
        );
    }

    #[test]
    fn non_adapter_error_classifies_as_unknown_transport() {
        let error = anyhow::Error::msg("something unrelated");
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::UnknownTransportError
        );
    }

    #[test]
    fn adapter_execute_surfaces_typed_connect_error() {
        // End-to-end through the public adapter API: a connect failure surfaces
        // as an AdapterError (wrapped in anyhow), classified to a safe category.
        let endpoint = idle_endpoint();
        let adapter = HttpConnectorAdapter::new(
            format!("http://127.0.0.1:{}/execute", endpoint.port),
            String::new(),
        );
        let invocation = ApprovedInvocation::new(
            crate::domain::InvocationIntent {
                invocation_id: crate::domain::InvocationId("test:connect".to_string()),
                run_id: crate::domain::RunId::new(),
                operation: crate::domain::operation::STDOUT_SEND_TEXT.to_string(),
                arguments: json!({ "text": "hi" }),
                idempotency_key: Some("test:connect".to_string()),
            },
            "decision:test".to_string(),
        );
        let error = adapter
            .execute(&invocation)
            .expect_err("expected connect failure");
        // Downcast confirms the typed error propagated through the trait.
        assert!(error.downcast_ref::<AdapterError>().is_some());
        assert_eq!(
            DispatchErrorCategory::from_error(&error),
            DispatchErrorCategory::UnknownTransportError
        );
    }

}
