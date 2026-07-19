//! Shadow Failure Proxy — controlled failure injection for Shadow Canary.
//!
//! This binary sits between the Kernel and the Deployment Harness:
//!   Kernel → Shadow Failure Proxy (:7400) → Deployment Harness (:7401)
//!
//! On the first N deployment calls (configured via env var), it returns a
//! protocol-legal failure receipt WITHOUT forwarding to the real Harness.
//! Subsequent calls are transparently forwarded.
//!
//! This avoids any runtime failure-injection code in the production
//! deployment-harness binary.
//!
//! Build (shadow-fixtures feature only):
//!   cargo build --release -p shadow-failure-proxy --features shadow-fixtures

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

const REAL_HARNESS_HOST: &str = "127.0.0.1";
const REAL_HARNESS_PORT: u16 = 7401;
const PROXY_PORT: u16 = 7400;

fn main() {
    let failure_count: usize = std::env::var("SHADOW_FAILURE_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let failure_retry_after: u64 = std::env::var("SHADOW_FAILURE_RETRY_AFTER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let injected = Mutex::new(FailureState {
        remaining: failure_count,
        retry_after_ms: failure_retry_after,
    });

    eprintln!(
        "[shadow-failure-proxy] listening on :{PROXY_PORT}, forwarding to {REAL_HARNESS_HOST}:{REAL_HARNESS_PORT}"
    );
    eprintln!(
        "[shadow-failure-proxy] failure_count={failure_count}, retry_after_ms={failure_retry_after}"
    );

    let listener = TcpListener::bind(format!("127.0.0.1:{PROXY_PORT}"))
        .expect("failed to bind proxy port");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let mut state = injected.lock().unwrap();
                if state.remaining > 0 {
                    state.remaining -= 1;
                    let remaining = state.remaining;
                    drop(state);
                    eprintln!(
                        "[shadow-failure-proxy] INJECTING FAILURE (remaining={remaining})"
                    );
                    if let Err(e) = handle_failure(&mut stream) {
                        eprintln!("[shadow-failure-proxy] failure handler error: {e}");
                    }
                } else {
                    drop(state);
                    if let Err(e) = handle_forward(&mut stream) {
                        eprintln!("[shadow-failure-proxy] forward error: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("[shadow-failure-proxy] accept error: {e}");
            }
        }
    }
}

struct FailureState {
    remaining: usize,
    retry_after_ms: u64,
}

/// Return a protocol-legal failure receipt without forwarding to the real Harness.
fn handle_failure(stream: &mut TcpStream) -> std::io::Result<()> {
    // Read the incoming HTTP request (minimal — just headers + body)
    let request = read_http_request(stream)?;

    // Only intercept POST /v1/deployments
    if !request.starts_with("POST /v1/deployments") {
        // Forward non-deployment requests
        return forward_request(&request, stream);
    }

    // Construct a protocol-legal failure receipt
    let failure_receipt = serde_json::json!({
        "protocol_version": "deployment.effect.v0",
        "receipt_id": format!("shadow_fail_{}", std::process::id()),
        "invocation_id": "shadow_fail_invocation",
        "intent_id": "shadow_fail_intent",
        "proposal_id": "shadow_fail_proposal",
        "decision_id": "shadow_fail_decision",
        "deployment_id": "shadow_fail_deployment",
        "component_id": "shadow-fail-component",
        "service_manifest_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "version": "0.1.0",
        "status": "SERVICE_EXITED_BEFORE_READY",
        "endpoint": "http://127.0.0.1:0",
        "health_status": "unhealthy",
        "log_ref": "shadow-fail/logs",
        "previous_artifact_digest": null,
        "started_at": chrono_now(),
        "finished_at": chrono_now(),
        "replayed": false,
    });

    let body = serde_json::to_string(&failure_receipt)?;
    let response = format!(
        "HTTP/1.1 422 Unprocessable Entity\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );

    stream.write_all(response.as_bytes())?;
    stream.flush()?;

    eprintln!("[shadow-failure-proxy] injected failure receipt (422)");
    Ok(())
}

/// Forward a request to the real Deployment Harness.
fn handle_forward(stream: &mut TcpStream) -> std::io::Result<()> {
    let request = read_http_request(stream)?;
    forward_request(&request, stream)
}

fn forward_request(request: &str, client_stream: &mut TcpStream) -> std::io::Result<()> {
    // Connect to the real deployment harness
    let mut upstream = TcpStream::connect(format!("{REAL_HARNESS_HOST}:{REAL_HARNESS_PORT}"))?;
    upstream.set_read_timeout(Some(Duration::from_secs(30)))?;

    // Forward the request
    upstream.write_all(request.as_bytes())?;
    upstream.flush()?;

    // Read the response
    let response = read_http_response(&mut upstream)?;

    // Send it back to the client (Kernel)
    client_stream.write_all(response.as_bytes())?;
    client_stream.flush()?;

    Ok(())
}

/// Read an entire HTTP request from a stream (headers + body).
fn read_http_request(stream: &mut TcpStream) -> std::io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = vec![0u8; 65536];
    let mut total_read = 0;

    // Read headers first
    loop {
        let n = stream.read(&mut buf[total_read..])?;
        if n == 0 {
            break;
        }
        total_read += n;
        if total_read >= 4 && buf[..total_read].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total_read >= buf.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
    }

    // Parse Content-Length
    let header_str = String::from_utf8_lossy(&buf[..total_read]);
    let content_length = header_str
        .lines()
        .find_map(|line| {
            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() == 2 && parts[0].trim().eq_ignore_ascii_case("content-length") {
                parts[1].trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    // Read body if needed
    let body_start = header_str.find("\r\n\r\n").map(|i| i + 4).unwrap_or(total_read);
    let body_received = total_read.saturating_sub(body_start);
    while body_received < content_length && total_read < buf.len() {
        let n = stream.read(&mut buf[total_read..])?;
        if n == 0 {
            break;
        }
        total_read += n;
    }

    Ok(String::from_utf8_lossy(&buf[..total_read]).to_string())
}

/// Read an HTTP response from a stream.
fn read_http_response(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = vec![0u8; 65536];
    let mut total_read = 0;

    loop {
        let n = stream.read(&mut buf[total_read..])?;
        if n == 0 {
            break;
        }
        total_read += n;
        if total_read >= 4 && buf[..total_read].windows(4).any(|w| w == b"\r\n\r\n") {
            // Parse Content-Length for body
            let header_str = String::from_utf8_lossy(&buf[..total_read]);
            let content_length = header_str
                .lines()
                .find_map(|line| {
                    let parts: Vec<&str> = line.splitn(2, ':').collect();
                    if parts.len() == 2 && parts[0].trim().eq_ignore_ascii_case("content-length") {
                        parts[1].trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            let body_start = header_str.find("\r\n\r\n").map(|i| i + 4).unwrap_or(total_read);
            while total_read < body_start + content_length {
                let n = stream.read(&mut buf[total_read..])?;
                if n == 0 {
                    break;
                }
                total_read += n;
            }
            break;
        }
        if total_read >= buf.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response too large",
            ));
        }
    }

    Ok(String::from_utf8_lossy(&buf[..total_read]).to_string())
}

fn chrono_now() -> String {
    // Simple RFC3339 without chrono dependency
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Format: 2026-07-19T12:00:00Z
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        secs / 31536000 + 1970,
        (secs % 31536000) / 2592000 + 1,
        (secs % 2592000) / 86400 + 1,
        (secs % 86400) / 3600,
        (secs % 3600) / 60,
        secs % 60,
    )
}
