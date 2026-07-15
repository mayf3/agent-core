//! Capability Change HTTP routing — path detection, Bearer auth, JSON body
//! parsing, handler dispatch, and CapabilityRouteError → HTTP status mapping.
//! This is a narrow extraction from `server/mod.rs` kept under 500 lines.

use crate::capabilities::store::ContentStore;
use crate::config::KernelConfig;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::Write;
use std::net::TcpStream;

/// Try to handle a capability request. Returns `Ok(true)` if the request was
/// a capability route (submit or decision) and a response was written.
/// Returns `Ok(false)` if the path did not match a capability route, so the
/// caller can try other routes. Returns `Err` only on write failures.
pub(crate) fn try_handle_capability_request(
    stream: &mut TcpStream,
    path: &str,
    method: &str,
    bearer: &str,
    body_bytes: &[u8],
    config: &KernelConfig,
    journal: &JournalStore,
    gateway: &Gateway,
) -> Result<bool> {
    if path == "/v1/capability-change-proposals" && method == "POST" {
        if !crate::server::capability_routes::capability_token_matches(
            bearer,
            &config.capability_submit_token,
        ) {
            let err = if config.capability_submit_token.is_none() {
                "capability_auth_not_configured"
            } else {
                "unauthorized"
            };
            return write_response(stream, 401, json!({"error": err})).map(|_| true);
        }
        let body: Value = match serde_json::from_slice(body_bytes) {
            Ok(b) => b,
            Err(_) => {
                return write_response(stream, 400, json!({"error":"invalid_json"})).map(|_| true);
            }
        };
        let principal = "capability_submitter";
        let result = crate::server::capability_routes::handle_submit_proposal(
            journal,
            gateway,
            &body,
            principal,
            &config.agent_id,
        )
        .map(|resp| serde_json::to_value(&resp).unwrap_or_default());
        let (status, resp_body) =
            match crate::server::capability_routes::map_capability_result(result) {
                Ok(t) => t,
                Err(e) => (500, json!({"ok": false, "error": e.to_string()})),
            };
        return write_response(stream, status, resp_body).map(|_| true);
    }

    if let Some(pid) = path
        .strip_prefix("/v1/capability-change-proposals/")
        .and_then(|s| s.strip_suffix("/decision"))
    {
        if !crate::server::capability_routes::capability_token_matches(
            bearer,
            &config.capability_decision_token,
        ) {
            let err = if config.capability_decision_token.is_none() {
                "capability_auth_not_configured"
            } else {
                "unauthorized"
            };
            return write_response(stream, 401, json!({"error": err})).map(|_| true);
        }
        let body: Value = match serde_json::from_slice(body_bytes) {
            Ok(b) => b,
            Err(_) => {
                return write_response(stream, 400, json!({"error":"invalid_json"})).map(|_| true);
            }
        };
        let store = ContentStore::new(config.harness_artifact_root.clone());
        let principal = "approval_workflow";
        let result = crate::server::capability_routes::handle_decision(
            journal,
            gateway,
            &store,
            pid,
            &body,
            principal,
            &config.agent_id,
        );
        let (status, resp_body) =
            match crate::server::capability_routes::map_capability_result(result) {
                Ok(t) => t,
                Err(e) => (500, json!({"ok": false, "error": e.to_string()})),
            };
        return write_response(stream, status, resp_body).map(|_| true);
    }

    // GET /v1/capability-change-proposals/{proposal_id} — read-only proposal query.
    // Uses the decision token (or submit token) for authorisation so that the
    // Feishu approval adapter can fetch digests before calling the decision API.
    if let Some(pid) = path.strip_prefix("/v1/capability-change-proposals/") {
        if pid.is_empty() || pid.contains('/') {
            return Ok(false);
        }
        let authed = crate::server::capability_routes::capability_token_matches(
            bearer,
            &config.capability_decision_token,
        ) || crate::server::capability_routes::capability_token_matches(
            bearer,
            &config.capability_submit_token,
        );
        if !authed {
            let err = if config.capability_decision_token.is_none()
                && config.capability_submit_token.is_none()
            {
                "capability_auth_not_configured"
            } else {
                "unauthorized"
            };
            return write_response(stream, 401, json!({"error": err})).map(|_| true);
        }
        let store = ContentStore::new(config.harness_artifact_root.clone());
        let result = crate::server::capability_routes::handle_get_proposal(journal, &store, pid);
        let (status, resp_body) =
            match crate::server::capability_routes::map_capability_result(result) {
                Ok(t) => t,
                Err(e) => (500, json!({"ok": false, "error": e.to_string()})),
            };
        return write_response(stream, status, resp_body).map(|_| true);
    }

    if let Some(remainder) = path.strip_prefix("/v1/components/") {
        let parts: Vec<&str> = remainder.split('/').collect();
        if method == "GET" && parts.len() == 1 {
            let authed = crate::server::capability_routes::capability_token_matches(
                bearer,
                &config.capability_decision_token,
            ) || crate::server::capability_routes::capability_token_matches(
                bearer,
                &config.capability_submit_token,
            );
            if !authed {
                return write_response(stream, 401, json!({"error":"unauthorized"})).map(|_| true);
            }
            let result = crate::server::component_control::observe(journal, parts[0]);
            let (status, response) =
                crate::server::capability_routes::map_capability_result(result)?;
            return write_response(stream, status, response).map(|_| true);
        }
        if method == "POST" && parts.len() == 2 && matches!(parts[1], "disable" | "rollback") {
            if !crate::server::capability_routes::capability_token_matches(
                bearer,
                &config.capability_decision_token,
            ) {
                return write_response(stream, 401, json!({"error":"unauthorized"})).map(|_| true);
            }
            let body: Value = match serde_json::from_slice(body_bytes) {
                Ok(body) => body,
                Err(_) => {
                    return write_response(stream, 400, json!({"error":"invalid_json"}))
                        .map(|_| true)
                }
            };
            let result = crate::server::component_control::handle(
                journal, config, parts[0], parts[1], &body,
            );
            let (status, response) =
                crate::server::capability_routes::map_capability_result(result)?;
            return write_response(stream, status, response).map(|_| true);
        }
        return write_response(stream, 404, json!({"error":"not_found"})).map(|_| true);
    }

    Ok(false)
}

/// Write a JSON HTTP response. Replicates the minimal logic from
/// server/mod.rs `write_json` without importing it.
fn write_response(stream: &mut TcpStream, status: u16, body: Value) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
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
