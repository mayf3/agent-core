//! Capability Host HTTP server.
//!
//! POST /execute — receive Kernel external invocation, execute artifact
//! GET /health  — health check

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use crate::artifact::{resolve_artifact, ArtifactError};
use crate::config::CapabilityHostConfig;
use crate::process::{run_artifact, ProcessError};
use crate::protocol;

/// Start the Capability Host HTTP server.
pub fn serve(config: CapabilityHostConfig) {
    let config = Arc::new(config);
    let listener = TcpListener::bind(&config.listen_addr).expect("capability host: bind failed");
    eprintln!("capability host listening on {}", config.listen_addr);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let config = Arc::clone(&config);
                thread::spawn(move || {
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
    let peer = stream.peer_addr().ok();
    let response = match read_http_request(&mut stream) {
        Ok((method, path, body)) => handle_request(method, path, &body, config),
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

fn read_http_request(stream: &mut TcpStream) -> Result<(String, String, String), String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("read request line failed: {e}"))?;

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("invalid request line".to_string());
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

    // Read headers to find Content-Length.
    let mut content_length: usize = 0;
    loop {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|e| format!("read header failed: {e}"))?;
        if header.trim().is_empty() {
            break;
        }
        if header.to_ascii_lowercase().starts_with("content-length:") {
            content_length = header
                .split(':')
                .nth(1)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
        }
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

    Ok((method, path, body))
}

pub(crate) fn handle_request(
    method: String,
    path: String,
    body: &str,
    config: &CapabilityHostConfig,
) -> String {
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => http_response(200, "OK", r#"{"status":"ok"}"#),
        ("POST", "/execute") => handle_execute(body, config),
        _ => http_response(404, "Not Found", r#"{"error":"not_found"}"#),
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

    // Resolve artifact by digest.
    let artifact_path = match resolve_artifact(&config.artifact_root, &req.artifact_digest) {
        Ok(path) => path,
        Err(ArtifactError::NotFound) => return ok_json(false, "artifact_not_found"),
        Err(ArtifactError::InvalidDigest) => return ok_json(false, "artifact_digest_invalid"),
        Err(ArtifactError::DigestMismatch) => return ok_json(false, "artifact_digest_mismatch"),
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
            let (ok, response_body_value) = protocol::map_process_response(&output.stdout);

            if !ok {
                return ok_json_from_value(response_body_value);
            }

            // Non-zero exit with ok response → artifact_failed.
            if output.exit_code != Some(0) {
                return ok_json(false, "artifact_failed");
            }

            ok_json_from_value(response_body_value)
        }
        Err(ProcessError::Timeout) => ok_json(false, "artifact_timeout"),
        Err(ProcessError::IoError(msg)) => ok_json(false, &format!("artifact_exec_error:{}", msg)),
    }
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
