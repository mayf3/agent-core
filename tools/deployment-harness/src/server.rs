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
    if request.bearer.as_deref() != Some(config.control_token.as_str()) {
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

fn safe_error(error: &anyhow::Error) -> (u16, &'static str) {
    let message = error.to_string();
    if message.contains("NOT_FOUND") {
        (404, "not_found")
    } else if message.contains("CONFLICT")
        || message.contains("TARGET_MISSING")
        || message.contains("NOT_MONOTONIC")
    {
        (409, "conflict")
    } else if message.contains("HEALTHCHECK") || message.contains("EXITED_BEFORE_READY") {
        (422, "healthcheck_failed")
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

    #[test]
    fn version_downgrade_is_a_definitive_conflict() {
        let error = anyhow::anyhow!("DEPLOYMENT_VERSION_NOT_MONOTONIC");
        assert_eq!(safe_error(&error), (409, "conflict"));
    }
}
