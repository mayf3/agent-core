//! Shadow Failure Proxy — controlled failure injection for Shadow Canary.
//!
//! This binary sits between the Kernel and the Deployment Harness:
//!   Kernel -> Shadow Failure Proxy (:7400) -> Deployment Harness (:7401)
//!
//! On the first N POST /v1/deployments calls (configured via SHADOW_FAILURE_COUNT),
//! it returns a protocol-legal definitive rejection WITHOUT forwarding to the
//! real Harness.  Non-deploy requests (health checks, disable, rollback, status
//! queries) are NEVER counted and are always transparently forwarded.
//!
//! This avoids any runtime failure-injection code in the production
//! deployment-harness binary.
//!
//! Build (shadow-fixtures feature only):
//!   cargo build --release -p shadow-failure-proxy --features shadow-fixtures
//!
//! Evidence counters (logged at exit):
//!   HEALTH_REQUEST_COUNT
//!   INJECTED_DEPLOYMENT_REQUEST_COUNT
//!   FORWARDED_DEPLOYMENT_REQUEST_COUNT

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const REAL_HARNESS_HOST: &str = "127.0.0.1";
const REAL_HARNESS_PORT: u16 = 7401;
const PROXY_PORT: u16 = 7400;

static HEALTH_COUNT: AtomicUsize = AtomicUsize::new(0);
static INJECTED_DEPLOY_COUNT: AtomicUsize = AtomicUsize::new(0);
static FORWARDED_DEPLOY_COUNT: AtomicUsize = AtomicUsize::new(0);

fn main() {
    let failure_count: usize = std::env::var("SHADOW_FAILURE_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let failure_retry_after: u64 = std::env::var("SHADOW_FAILURE_RETRY_AFTER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let state = Mutex::new(FailureState {
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
                // Step 1: Read the FULL request (method + path + headers + body)
                let request = match read_http_request(&mut stream) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("[shadow-failure-proxy] read error: {e}");
                        continue;
                    }
                };

                // Step 2: Classify the request
                // Health checks and non-deploy requests are ALWAYS forwarded
                // without consuming the failure budget.
                if request.starts_with("GET /health ") {
                    HEALTH_COUNT.fetch_add(1, Ordering::SeqCst);
                    forward_request(&request, &mut stream);
                } else if request.starts_with("POST /v1/deployments") {
                    // POST /v1/deployments -> possibly inject failure
                    let mut state = state.lock().unwrap();
                    if state.remaining > 0 {
                        state.remaining -= 1;
                        let remaining = state.remaining;
                        drop(state);
                        INJECTED_DEPLOY_COUNT.fetch_add(1, Ordering::SeqCst);
                        eprintln!(
                            "[shadow-failure-proxy] INJECTING FAILURE on deploy (remaining={remaining})"
                        );
                        handle_deploy_failure(&request, &mut stream);
                    } else {
                        drop(state);
                        FORWARDED_DEPLOY_COUNT.fetch_add(1, Ordering::SeqCst);
                        forward_request(&request, &mut stream);
                    }
                } else {
                    // All other requests (disable, rollback, status, etc.)
                    // are forwarded without consuming failure budget.
                    forward_request(&request, &mut stream);
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

/// Return a protocol-legal definitive rejection for a POST /v1/deployments
/// request.  The Kernel's DeploymentHarnessClient::post() treats HTTP 422
/// responses with {"ok":false,"error_code":"..."} as DefinitiveDeploymentRejection,
/// which causes fail_trusted_activation_atomic() to record ActivationFailed.
fn handle_deploy_failure(request: &str, stream: &mut TcpStream) {
    let body = serde_json::json!({
        "protocol_version": "deployment.effect.v0",
        "ok": false,
        "error_code": "service_unhealthy",
    });

    let body_str = serde_json::to_string(&body).unwrap_or_default();
    let response = format!(
        "HTTP/1.1 422 Unprocessable Entity\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body_str.len(),
        body_str
    );

    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    eprintln!("[shadow-failure-proxy] injected definitive rejection (422) for deploy");
}

/// Forward a request to the real Deployment Harness.
fn forward_request(request: &str, client_stream: &mut TcpStream) {
    let result = (|| -> std::io::Result<()> {
        let mut upstream =
            TcpStream::connect(format!("{REAL_HARNESS_HOST}:{REAL_HARNESS_PORT}"))?;
        upstream.set_read_timeout(Some(Duration::from_secs(30)))?;
        upstream.write_all(request.as_bytes())?;
        upstream.flush()?;
        let response = read_http_response(&mut upstream)?;
        client_stream.write_all(response.as_bytes())?;
        client_stream.flush()?;
        Ok(())
    })();

    if let Err(e) = result {
        eprintln!("[shadow-failure-proxy] forward error: {e}");
    }
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
    while total_read < body_start + content_length && total_read < buf.len() {
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
