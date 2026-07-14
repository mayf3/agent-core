//! HTTP handler for the `event.observe.v0` pull endpoint.
//!
//! ## Endpoint
//!
//! **`GET /v1/events`** — pull journal events after a cursor with optional filters.
//!
//! Query parameters (all optional):
//!
//! | Parameter      | Type    | Description                                        |
//! |----------------|---------|----------------------------------------------------|
//! | `cursor`       | i64     | Return events with sequence > this value (default 0) |
//! | `limit`        | i64     | Max events per page (1–1000, default 100)          |
//! | `event_kind`   | string  | Exact kind filter                                  |
//! | `run_id`       | string  | Exact run ID filter                                |
//! | `session_id`   | string  | Exact session ID filter                            |
//! | `principal_id` | string  | Exact principal ID filter (resolved via runs JOIN) |
//!
//! Authentication: Bearer token matching `AGENT_CORE_IPC_TOKEN`.
//!
//! ## Response
//!
//! ```json
//! {
//!   "schema_version": "event.observe.v0",
//!   "events": [ ... ],
//!   "next_cursor": 42,
//!   "has_more": false
//! }
//! ```
//!
//! Error responses:
//! - 400: `invalid_limit`, `invalid_cursor`
//! - 401: `unauthorized`
//! - 503: `journal_corrupt`

use crate::config::KernelConfig;
use crate::journal::event_observe::{EventObserveQuery, MAX_OBSERVE_LIMIT};
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;

/// Try to handle an event-observe request.
///
/// Returns `Ok(true)` if the request matched the events endpoint and a
/// response was written. Returns `Ok(false)` if the path did not match.
pub(crate) fn try_handle_event_observe(
    stream: &mut TcpStream,
    path: &str,
    method: &str,
    bearer: &str,
    body_bytes: &[u8],
    config: &KernelConfig,
    journal: &JournalStore,
) -> Result<bool> {
    // Match path (strip query string for matching)
    let path_no_qs = path.split('?').next().unwrap_or("");
    if path_no_qs != "/v1/events" {
        return Ok(false);
    }

    // Both GET and POST accepted
    if method != "GET" && method != "POST" {
        return Ok(false);
    }

    // Require IPC auth
    if bearer != config.ipc_token.as_str() {
        return write_response(stream, 401, json!({"ok": false, "error": "unauthorized"}))
            .map(|_| true);
    }

    // Parse query parameters from GET query string or POST body
    let params = if method == "GET" {
        parse_query_params(path)
    } else {
        parse_json_body(body_bytes)
    };

    // Build EventObserveQuery
    let query = build_query(params)?;

    // Execute observe
    match journal.observe_events(&query) {
        Ok(resp) => write_response(stream, 200, serde_json::to_value(&resp)?).map(|_| true),
        Err(e) => {
            let msg = e.to_string();
            let (status, err_key) = if msg.contains("journal_corrupt") {
                (503, "journal_corrupt")
            } else if msg.contains("invalid_limit") {
                (400, "invalid_limit")
            } else if msg.contains("invalid_cursor") {
                (400, "invalid_cursor")
            } else {
                (500, "internal_error")
            };
            write_response(stream, status, json!({"ok": false, "error": err_key})).map(|_| true)
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter parsing
// ---------------------------------------------------------------------------

fn parse_query_params(path: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(query) = path.split('?').nth(1) {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                // URL-decode percent-encoded values (basic version)
                let decoded = percent_decode(v);
                map.insert(k.to_string(), decoded);
            }
        }
    }
    map
}

fn parse_json_body(body_bytes: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(val) = serde_json::from_slice::<Value>(body_bytes) {
        if let Value::Object(obj) = val {
            for (k, v) in obj {
                let s = match v {
                    Value::String(s) => s,
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "".to_string(),
                    _ => continue,
                };
                map.insert(k, s);
            }
        }
    }
    map
}

/// Basic percent-decoding (handles %XX only, not UTF-8 multi-byte).
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            // Invalid percent-encoding, preserve original
            result.push('%');
            result.push_str(&hex);
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}

fn build_query(params: HashMap<String, String>) -> Result<EventObserveQuery> {
    let limit_str = params.get("limit").map(|s| s.as_str()).unwrap_or("");
    let cursor_str = params.get("cursor").map(|s| s.as_str()).unwrap_or("");

    let limit: i64 = if limit_str.is_empty() {
        crate::journal::event_observe::DEFAULT_OBSERVE_LIMIT
    } else {
        limit_str.parse().map_err(|_| anyhow::anyhow!("invalid_limit"))?
    };

    if limit < 1 || limit > MAX_OBSERVE_LIMIT {
        bail!("invalid_limit");
    }

    let after_sequence: Option<i64> = if cursor_str.is_empty() {
        None
    } else {
        let seq: i64 = cursor_str.parse().map_err(|_| anyhow::anyhow!("invalid_cursor"))?;
        if seq < 0 {
            bail!("invalid_cursor");
        }
        Some(seq)
    };

    Ok(EventObserveQuery {
        after_sequence,
        limit,
        event_kind: params.get("event_kind").cloned().unwrap_or_default(),
        run_id: params.get("run_id").cloned().unwrap_or_default(),
        session_id: params.get("session_id").cloned().unwrap_or_default(),
        principal_id: params.get("principal_id").cloned().unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Response writing
// ---------------------------------------------------------------------------

fn write_response(stream: &mut TcpStream, status: u16, body: Value) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let payload = serde_json::to_string(&body)?;
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}
