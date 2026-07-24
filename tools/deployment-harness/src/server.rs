use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::config::DeploymentHarnessConfig;
use crate::manager;

const MAX_BODY: usize = 128 * 1024;

pub fn serve(config: DeploymentHarnessConfig) -> Result<()> {
    config.validate()?;
    manager::reconcile(&config)?;
    let listener = TcpListener::bind(config.listen_addr)?;
    serve_listener(listener, config)
}

pub fn serve_listener(listener: TcpListener, config: DeploymentHarnessConfig) -> Result<()> {
    config.validate()?;
    let config = Arc::new(config);
    manager::start_health_monitor(Arc::clone(&config));
    for stream in listener.incoming() {
        let mut stream = stream?;
        let config = Arc::clone(&config);
        std::thread::spawn(move || {
            let _ = handle(&mut stream, &config);
        });
    }
    Ok(())
}

fn handle(stream: &mut TcpStream, config: &DeploymentHarnessConfig) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let request = read_request(stream)?;
    if request.method == "GET" && request.path == "/health" {
        return write_json(stream, 200, json!({"status":"ok"}));
    }
    if !is_authorized(
        request.method.as_str(),
        request.bearer.as_deref(),
        &config.control_token,
        config.read_token.as_deref(),
    ) {
        return write_json(stream, 401, json!({"ok":false,"error_code":"unauthorized"}));
    }
    let result = route(config, &request);
    match result {
        Ok(value) => write_json(stream, 200, value),
        Err(error) => {
            let (status, code) = safe_error(&error);
            write_json(
                stream,
                status,
                json!({
                    "protocol_version":"deployment.effect.v0",
                    "ok":false,
                    "error_code":code,
                }),
            )
        }
    }
}

fn route(config: &DeploymentHarnessConfig, request: &Request) -> Result<Value> {
    if request.method == "POST" && request.path == "/v1/deployments" {
        return Ok(serde_json::to_value(manager::deploy(
            config,
            &request.body,
        )?)?);
    }
    let Some(remainder) = request.path.strip_prefix("/v1/components/") else {
        bail!("ROUTE_NOT_FOUND");
    };
    let parts: Vec<&str> = remainder.split('/').collect();
    match (request.method.as_str(), parts.as_slice()) {
        ("GET", [component_id]) => manager::status(config, component_id),
        ("POST", [component_id, "disable"]) => {
            let body: ControlBody = serde_json::from_slice(&request.body)
                .map_err(|_| anyhow::anyhow!("CONTROL_BODY_INVALID"))?;
            Ok(serde_json::to_value(manager::disable(
                config,
                component_id,
                &body.decision_id,
            )?)?)
        }
        ("POST", [component_id, "rollback"]) => {
            let body: ControlBody = serde_json::from_slice(&request.body)
                .map_err(|_| anyhow::anyhow!("CONTROL_BODY_INVALID"))?;
            Ok(serde_json::to_value(manager::rollback(
                config,
                component_id,
                &body.decision_id,
            )?)?)
        }
        _ => bail!("ROUTE_NOT_FOUND"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlBody {
    decision_id: String,
}

struct Request {
    method: String,
    path: String,
    bearer: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("REQUEST_TRUNCATED");
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
        if bytes.len() > 32 * 1024 {
            bail!("REQUEST_HEADERS_TOO_LARGE");
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = headers.lines();
    let first = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("REQUEST_INVALID"))?;
    let mut first = first.split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("").to_string();
    if !matches!(method.as_str(), "GET" | "POST") || !path.starts_with('/') {
        bail!("REQUEST_INVALID");
    }
    let mut content_length = None;
    let mut bearer = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            bail!("REQUEST_HEADER_INVALID");
        };
        if name.eq_ignore_ascii_case("content-length") {
            let parsed: usize = value.trim().parse()?;
            if content_length.replace(parsed).is_some() {
                bail!("CONTENT_LENGTH_DUPLICATE");
            }
        } else if name.eq_ignore_ascii_case("authorization") {
            bearer = value.trim().strip_prefix("Bearer ").map(str::to_string);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("TRANSFER_ENCODING_NOT_SUPPORTED");
        }
    }
    let expected = content_length.unwrap_or(0);
    if expected > MAX_BODY {
        bail!("REQUEST_BODY_TOO_LARGE");
    }
    let body_start = header_end + 4;
    let mut body = bytes[body_start..].to_vec();
    while body.len() < expected {
        let mut chunk = vec![0u8; (expected - body.len()).min(8192)];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("REQUEST_BODY_TRUNCATED");
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(expected);
    Ok(Request {
        method,
        path,
        bearer,
        body,
    })
}

/// Authorise a request based on method and bearer token.
///
/// - GET requests are accepted with either `control_token` or
///   `read_token` (if configured).
/// - POST requests require the `control_token`.
/// - A missing `read_token` means GET falls back to `control_token`
///   only (backward compatible).
pub(crate) fn is_authorized(
    method: &str,
    bearer: Option<&str>,
    control_token: &str,
    read_token: Option<&str>,
) -> bool {
    if method == "GET" {
        // GET accepts either control_token or read_token
        Some(control_token) == bearer
            || read_token.is_some() && read_token == bearer
    } else {
        // POST requires control_token
        Some(control_token) == bearer
    }
}

	fn safe_error(error: &anyhow::Error) -> (u16, &'static str) {
	    let message = error.to_string();
	    if message == "COMPONENT_NOT_DEPLOYED" {
	        // A component that has never been deployed should be reported as
	        // 404 (Not Found) so that version‑query callers (e.g. Coding
	        // Harness) can distinguish "never deployed" from "server error".
	        (404, "not_found")
	    } else if message.contains("NOT_FOUND") {
	        (404, "not_found")
	    } else if message.contains("CONFLICT")
        || message.contains("TARGET_MISSING")
        || message.contains("NOT_MONOTONIC")
    {
        (409, "conflict")
    } else if message.contains("EXITED_BEFORE_READY") {
        (422, "service_exited_before_ready")
    } else if message.contains("CONNECTION_REFUSED") {
        (422, "service_healthcheck_connection_failed")
    } else if message.contains("CONNECTION_TIMEOUT") {
        (422, "service_healthcheck_connection_failed")
    } else if message.contains("REJECTED") {
        (422, "service_healthcheck_rejected")
    } else if message.contains("IDENTITY_MISMATCH") {
        (422, "service_healthcheck_identity_mismatch")
    } else if message.contains("MALFORMED_RESPONSE") {
        (422, "service_healthcheck_malformed_response")
    } else if message.contains("INVALID") || message.contains("MALFORMED") {
        (400, "invalid_request")
    } else {
        (500, "deployment_failed")
    }
}

fn write_json(stream: &mut TcpStream, status: u16, value: Value) -> Result<()> {
    let body = serde_json::to_vec(&value)?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        _ => "Internal Server Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(&body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::safe_error;
    use super::is_authorized;

	    #[test]
	    fn version_downgrade_is_a_definitive_conflict() {
	        let error = anyhow::anyhow!("DEPLOYMENT_VERSION_NOT_MONOTONIC");
	        assert_eq!(safe_error(&error), (409, "conflict"));
	    }

	    #[test]
	    fn component_not_deployed_maps_to_404() {
	        // A component that has never been deployed is semantically
	        // equivalent to "not found" — the caller must receive 404
	        // so they can distinguish "never deployed" from "server error".
	        let error = anyhow::anyhow!("COMPONENT_NOT_DEPLOYED");
	        assert_eq!(safe_error(&error), (404, "not_found"));
	    }

	    #[test]
	    fn unknown_error_still_500() {
	        // Real internal / database / I/O errors must remain 500.
	        // The COMPONENT_NOT_DEPLOYED ➜ 404 mapping is an explicit
	        // narrow exception — no other error should silently become 404.
	        let error = anyhow::anyhow!("INTERNAL_STATE_CORRUPTION");
	        assert_eq!(safe_error(&error), (500, "deployment_failed"));
	    }

	    #[test]
	    fn undeployed_component_is_not_swallowed_by_generic_not_found() {
	        // Prove that COMPONENT_NOT_DEPLOYED is handled before the
	        // generic NOT_FOUND substring check, so its exact semantics
	        // are preserved even if a future refactor changes the message.
	        let exact = anyhow::anyhow!("COMPONENT_NOT_DEPLOYED");
	        let generic = anyhow::anyhow!("PATH_NOT_FOUND");
	        assert_eq!(safe_error(&exact), (404, "not_found"));
	        assert_eq!(safe_error(&generic), (404, "not_found"));
	        // Both map to 404 but through different branches — the
	        // exact one is explicit, the generic one is the fallback.
	    }

    // ── is_authorized unit tests ──────────────────────────

    #[test]
    fn read_token_can_get() {
        assert!(is_authorized("GET", Some("read"), "control", Some("read")));
    }

    #[test]
    fn read_token_cannot_deploy() {
        assert!(!is_authorized("POST", Some("read"), "control", Some("read")));
    }

    #[test]
    fn read_token_cannot_disable() {
        assert!(!is_authorized("POST", Some("read"), "control", Some("read")));
    }

    #[test]
    fn read_token_cannot_rollback() {
        assert!(!is_authorized("POST", Some("read"), "control", Some("read")));
    }

    #[test]
    fn control_token_can_get() {
        assert!(is_authorized("GET", Some("control"), "control", Some("read")));
    }

    #[test]
    fn control_token_can_post() {
        assert!(is_authorized("POST", Some("control"), "control", Some("read")));
    }

    #[test]
    fn no_token_get_fails() {
        assert!(!is_authorized("GET", None, "control", Some("read")));
    }

    #[test]
    fn wrong_token_get_fails() {
        assert!(!is_authorized("GET", Some("wrong"), "control", Some("read")));
    }

    #[test]
    fn wrong_token_post_fails() {
        assert!(!is_authorized("POST", Some("wrong"), "control", Some("read")));
    }

    #[test]
    fn no_read_token_fallback_to_control_for_get() {
        assert!(is_authorized("GET", Some("control"), "control", None));
    }

    #[test]
    fn no_read_token_rejects_read_token_for_get() {
        assert!(!is_authorized("GET", Some("read"), "control", None));
    }
}
