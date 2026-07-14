//! Capability Host HTTP server.
//!
//! POST /deploy  — prepare a trusted calculator deployment
//! POST /execute — receive Kernel external invocation, execute deployed artifact
//! GET /health   — health check

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::artifact::{resolve_artifact, ArtifactError};
use crate::config::CapabilityHostConfig;
use crate::deployment;
use crate::process::{run_artifact, ProcessError};
use crate::protocol;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_REQUESTS: usize = 32;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADER_LINE_BYTES: usize = 4 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Start the Capability Host HTTP server.
pub fn serve(config: CapabilityHostConfig) {
    config
        .validate()
        .expect("capability host: invalid configuration");
    let listener = TcpListener::bind(&config.listen_addr).expect("capability host: bind failed");
    serve_listener(config, listener);
}

/// Run the production accept loop on an already-bound listener. This is used
/// by tests to avoid a bind-after-port-selection race.
pub fn serve_listener(config: CapabilityHostConfig, listener: TcpListener) {
    config
        .validate()
        .expect("capability host: invalid configuration");
    let config = Arc::new(config);
    let in_flight = Arc::new(AtomicUsize::new(0));
    eprintln!("capability host listening on {}", config.listen_addr);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if in_flight.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_REQUESTS {
                    in_flight.fetch_sub(1, Ordering::AcqRel);
                    reject_overloaded(stream);
                    continue;
                }
                let config = Arc::clone(&config);
                let in_flight = Arc::clone(&in_flight);
                thread::spawn(move || {
                    let _guard = RequestGuard(in_flight);
                    handle(stream, &config);
                });
            }
            Err(e) => {
                eprintln!("capability host: accept failed: {e}");
            }
        }
    }
}

pub fn handle(mut stream: TcpStream, config: &CapabilityHostConfig) {
    if stream.set_read_timeout(Some(IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(IO_TIMEOUT)).is_err()
    {
        return;
    }
    let peer = stream.peer_addr().ok();
    let response = match read_http_request(&mut stream) {
        Ok((method, path, authorization, body)) => {
            handle_request(method, path, authorization.as_deref(), &body, config)
        }
        Err(e) => http_response(400, "Bad Request", &e),
    };
    let _ = stream.write_all(response.as_bytes());
    if let Some(addr) = peer {
        eprintln!(
            "capability host: {} -> {}",
            addr,
            response.lines().next().unwrap_or("")
        );
    }
}

fn read_http_request(
    stream: &mut TcpStream,
) -> Result<(String, String, Option<String>, String), String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut header_bytes = 0;
    let request_line = read_header_line(&mut reader, &mut header_bytes)?;

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("invalid request line".to_string());
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

    // Read headers to find Content-Length.
    let mut content_length = None;
    let mut authorization = None;
    let mut header_terminated = false;
    for _ in 0..MAX_HEADERS {
        let header = read_header_line(&mut reader, &mut header_bytes)?;
        if header.trim().is_empty() {
            header_terminated = true;
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| "malformed header".to_string())?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err("duplicate content-length header".into());
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "invalid content-length".to_string())?,
            );
        } else if name.eq_ignore_ascii_case("authorization") {
            if authorization.is_some() {
                return Err("duplicate authorization header".into());
            }
            authorization = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("transfer-encoding is unsupported".into());
        }
    }
    if !header_terminated {
        return Err("too many headers".into());
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err("request body too large".into());
    }

    // Read body.
    let mut body = String::new();
    if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("read body failed: {e}"))?;
        body = String::from_utf8(buf).map_err(|_| "body not UTF-8".to_string())?;
    }

    Ok((method, path, authorization, body))
}

fn read_header_line(reader: &mut impl BufRead, total: &mut usize) -> Result<String, String> {
    let mut line = String::new();
    let read = reader
        .take((MAX_HEADER_LINE_BYTES + 1) as u64)
        .read_line(&mut line)
        .map_err(|error| format!("read header failed: {error}"))?;
    if read == 0 || read > MAX_HEADER_LINE_BYTES || !line.ends_with('\n') {
        return Err("header line too long or incomplete".into());
    }
    *total = total.saturating_add(read);
    if *total > MAX_HEADER_BYTES {
        return Err("headers too large".into());
    }
    Ok(line)
}

fn reject_overloaded(mut stream: TcpStream) {
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let response = http_response(
        503,
        "Service Unavailable",
        r#"{"ok":false,"error_code":"server_busy"}"#,
    );
    let _ = stream.write_all(response.as_bytes());
}

struct RequestGuard(Arc<AtomicUsize>);

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn handle_request(
    method: String,
    path: String,
    authorization: Option<&str>,
    body: &str,
    config: &CapabilityHostConfig,
) -> String {
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => http_response(200, "OK", r#"{"status":"ok"}"#),
        ("POST", "/deploy") | ("POST", "/v1/deployments/prepare") => {
            if !bearer_matches(authorization, &config.control_token) {
                return http_response(
                    401,
                    "Unauthorized",
                    r#"{"ok":false,"error_code":"unauthorized"}"#,
                );
            }
            handle_deploy(body, config)
        }
        ("POST", "/execute") => {
            if !bearer_matches(authorization, &config.execution_token) {
                return http_response(
                    401,
                    "Unauthorized",
                    r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"unauthorized"}"#,
                );
            }
            handle_execute(body, config)
        }
        _ => http_response(404, "Not Found", r#"{"error":"not_found"}"#),
    }
}

fn handle_deploy(body: &str, config: &CapabilityHostConfig) -> String {
    match deployment::prepare(config, body) {
        Ok(value) => http_response(
            200,
            "OK",
            &serde_json::to_string(&value).unwrap_or_default(),
        ),
        Err(error) => {
            let status = match &error {
                deployment::DeployError::State => 500,
                deployment::DeployError::BindingMismatch | deployment::DeployError::Conflict => 409,
                _ => 400,
            };
            let body = serde_json::json!({
                "protocol_version":"capability-deploy-v1",
                "ok":false,
                "error_code":error.code(),
            });
            http_response(
                status,
                if status == 500 {
                    "Internal Server Error"
                } else if status == 409 {
                    "Conflict"
                } else {
                    "Bad Request"
                },
                &serde_json::to_string(&body).unwrap_or_default(),
            )
        }
    }
}

pub(crate) fn handle_execute(body: &str, config: &CapabilityHostConfig) -> String {
    // Parse incoming request body.
    let body_json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return http_response(
                200,
                "OK",
                r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"malformed_request"}"#,
            )
        }
    };

    let req = match protocol::parse_harness_request(&body_json) {
        Ok(r) => r,
        Err(msg) => {
            return http_response(
                200,
                "OK",
                &format!(
                    r#"{{"protocol_version":"external-harness-v1","ok":false,"error_code":"{}"}}"#,
                    msg
                ),
            )
        }
    };

    let deployment = match deployment::authorize_execution(config, &req) {
        Ok(record) => record,
        Err(error) => return ok_json(false, error.code()),
    };

    // Resolve artifact by digest.
    let artifact_path = match resolve_artifact(&config.artifact_root, &req.artifact_digest) {
        Ok(path) => path,
        Err(ArtifactError::NotFound) => return ok_json(false, "artifact_not_found"),
        Err(ArtifactError::InvalidDigest) => return ok_json(false, "artifact_digest_invalid"),
        Err(ArtifactError::DigestMismatch) => return ok_json(false, "artifact_digest_mismatch"),
        Err(ArtifactError::UnsafeMaterializationRoot | ArtifactError::MaterializationChanged) => {
            return ok_json(false, "artifact_materialization_unsafe")
        }
        Err(ArtifactError::StoreError(msg)) => {
            return ok_json(false, &format!("artifact_store_error:{}", msg))
        }
    };

    // Build process request JSON for artifact stdin.
    let process_req = protocol::build_process_request(&req);
    let stdin_json = match serde_json::to_string(&process_req) {
        Ok(j) => j,
        Err(_) => {
            return ok_json(false, "internal_error");
        }
    };

    // Execute artifact.
    let result = run_artifact(
        &artifact_path,
        &stdin_json,
        config.exec_timeout,
        config.max_stdout_bytes,
        config.max_stderr_bytes,
    );

    match result {
        Ok(output) => {
            // Map process output to external-harness-v1 response.
            let (ok, mut response_body_value) = protocol::map_process_response(&output.stdout);

            if !ok {
                return ok_json_from_value(response_body_value);
            }

            // Non-zero exit with ok response → artifact_failed.
            if output.exit_code != Some(0) {
                return ok_json(false, "artifact_failed");
            }

            if let Some(object) = response_body_value.as_object_mut() {
                object.insert(
                    "capability_host_execution_id".into(),
                    serde_json::Value::String(deployment::execution_id(
                        &deployment.deployment_id,
                        &req.invocation_id,
                        &req.arguments,
                    )),
                );
            }

            ok_json_from_value(response_body_value)
        }
        Err(ProcessError::Timeout) => ok_json(false, "artifact_timeout"),
        Err(ProcessError::IoError(msg)) => ok_json(false, &format!("artifact_exec_error:{}", msg)),
    }
}

fn bearer_matches(header: Option<&str>, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let supplied = match header.and_then(|value| value.strip_prefix("Bearer ")) {
        Some(value) if !value.is_empty() => value.as_bytes(),
        _ => return false,
    };
    let expected = expected.as_bytes();
    let mut difference = supplied.len() ^ expected.len();
    let length = supplied.len().max(expected.len());
    for index in 0..length {
        difference |= usize::from(
            supplied.get(index).copied().unwrap_or(0) ^ expected.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

/// Build an HTTP response string.
fn http_response(status_code: u16, status_text: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_code} {status_text}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
    )
}

/// Build an external-harness-v1 success/failure response.
pub(crate) fn ok_json(ok: bool, error_code: &str) -> String {
    let body = if ok {
        format!(r#"{{"protocol_version":"external-harness-v1","ok":true,"result":null}}"#)
    } else {
        format!(
            r#"{{"protocol_version":"external-harness-v1","ok":false,"error_code":"{error_code}"}}"#
        )
    };
    http_response(200, "OK", &body)
}

pub(crate) fn ok_json_from_value(value: serde_json::Value) -> String {
    let body = serde_json::to_string(&value).unwrap_or_default();
    http_response(200, "OK", &body)
}
