//! Read-only version query against the Deployment Harness.
//!
//! Uses a dedicated read-only token (`AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN`)
//! that is scoped to only allow `GET /v1/components/{id}`.  Every response
//! is parsed with **fail-closed** semantics — any status other than 404 or
//! a well-formed 200 with `ok:true` and a non-empty `version` results in
//! an `Err`.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Environment variable for the DH read‑only URL (falls back to
/// `AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL` when not set).
const ENV_DH_READ_URL: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_READ_URL";
const ENV_DH_CONTROL_URL_FALLBACK: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL";
const DEFAULT_DH_URL: &str = "http://127.0.0.1:7400";

/// Environment variable for the DH read‑only token.
///
/// This token MUST be scoped to only allow `GET /v1/components/{id}`.
/// It must NOT permit deploy, disable, rollback, or any other write
/// operation.  The Deployment Harness enforces this server-side.
const ENV_DH_READ_TOKEN: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN";

/// Query the Deployment Harness for the current installed version of
/// a component.
///
/// # Semantics (fail‑closed)
///
/// | HTTP status | JSON body | Result |
/// |-------------|-----------|--------|
/// | 404         | any       | `Ok(None)` — component does not exist |
/// | 200         | `{"ok":true,"version":"X.Y.Z"}` | `Ok(Some("X.Y.Z"))` |
/// | 200         | missing `version` or empty | `Err` |
/// | 200         | `ok` is not `true` | `Err` |
/// | 401, 403, 409, 429, 5xx | any | `Err` |
/// | transport / timeout / JSON parse | — | `Err` |
///
/// Every error is fail‑closed: the caller MUST NOT interpret a non‑404
/// error as "component does not exist" or silently fall back to a
/// default version.
pub fn query_deployed_version(component_id: &str) -> Result<Option<String>> {
    let base_url = std::env::var(ENV_DH_READ_URL)
        .or_else(|_| std::env::var(ENV_DH_CONTROL_URL_FALLBACK))
        .unwrap_or_else(|_| DEFAULT_DH_URL.to_string());
    let token = std::env::var(ENV_DH_READ_TOKEN)
        .map_err(|_| anyhow!("MISSING_DH_READ_TOKEN"))?;
    if token.len() < 32 {
        return Err(anyhow!("DH_READ_TOKEN_TOO_SHORT"));
    }

    let url = url_parse(&base_url)?;
    let path = format!("{}/v1/components/{}", url.path, component_id);

    let addr = format!("{}:{}", url.host, url.port);
    let mut stream = TcpStream::connect_timeout(
        &addr.parse().map_err(|e| anyhow!("BAD_ADDR: {e}"))?,
        Duration::from_secs(5),
    )
    .map_err(|e| anyhow!("DH_CONNECT: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok();

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n",
        path = path,
        host = url.host,
        token = token,
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| anyhow!("DH_WRITE: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| anyhow!("DH_READ: {e}"))?;

    // ── 1. Parse HTTP status line ──
    let status_code = parse_status_code(&raw)?;

    // ── 2. Fail‑closed dispatch by status ──
    match status_code {
        404 => Ok(None),
        200 => {
            let body = extract_http_body(&raw);
            let resp: Value =
                serde_json::from_slice(body).map_err(|e| anyhow!("DH_JSON: {e}"))?;
            if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Err(anyhow!("DH_NOT_OK"));
            }
            let version = resp
                .get("version")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("DH_MISSING_VERSION"))?;
            Ok(Some(version.to_string()))
        }
        other => Err(anyhow!("DH_UNEXPECTED_STATUS:{other}")),
    }
}

/// Parse the HTTP status code from a raw HTTP/1.1 response.
pub(crate) fn parse_status_code(raw: &[u8]) -> Result<u16> {
    let line_end = raw
        .windows(2)
        .position(|w| w == b"\r\n")
        .ok_or_else(|| anyhow!("DH_NO_STATUS_LINE"))?;
    let status_line = std::str::from_utf8(&raw[..line_end])
        .map_err(|_| anyhow!("DH_STATUS_NOT_UTF8"))?;
    let code_str = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("DH_STATUS_MALFORMED:{status_line}"))?;
    code_str
        .parse::<u16>()
        .map_err(|_| anyhow!("DH_STATUS_NOT_NUMERIC:{code_str}"))
}

struct ParsedUrlOwned {
    host: String,
    port: u16,
    path: String,
}

fn url_parse(raw: &str) -> Result<ParsedUrlOwned> {
    let without_scheme = raw
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("DH_URL_MUST_BE_HTTP"))?;
    let (host_port, path) = match without_scheme.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (without_scheme, String::new()),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>().map_err(|e| anyhow!("BAD_PORT: {e}"))?,
        ),
        None => (host_port.to_string(), 7400u16),
    };
    Ok(ParsedUrlOwned { host, port, path })
}

fn extract_http_body(raw: &[u8]) -> &[u8] {
    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
        &raw[pos + 4..]
    } else {
        raw
    }
}
