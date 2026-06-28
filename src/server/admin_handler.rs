//! Admin HTTP handler for the harness control plane.
//! Routes are only available when `AGENT_CORE_HARNESS_ADMIN_TOKEN` is set.

use crate::config::KernelConfig;
use crate::journal::JournalStore;
use serde_json::{json, Value};
use std::net::TcpStream;

use super::{write_json, HttpRequest};

pub fn handle_admin_request(
    stream: &mut TcpStream,
    config: &KernelConfig,
    journal: &JournalStore,
    request: &HttpRequest,
) -> Result<(), anyhow::Error> {
    use crate::harness::admin;
    if !admin::is_admin_enabled(config) {
        return write_json(
            stream,
            401,
            json!({"ok": false, "error": "admin_not_configured"}),
        );
    }
    if admin::validate_admin_token(config, request.bearer_token.as_deref()).is_err() {
        return write_json(stream, 401, json!({"ok": false, "error": "unauthorized"}));
    }
    let path = request
        .path
        .trim_start_matches("/v1/admin")
        .trim_end_matches('/');
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(v) => v,
        Err(_) => Value::Null,
    };
    let result: Result<Value, anyhow::Error> = match (request.method.as_str(), path) {
        ("POST", "/harness/bundles") => admin::handle_register_bundle(journal, &body),
        ("GET", "/harness/bundles") => admin::handle_list_bundles(journal),
        ("GET", "/harness/registrations") => admin::handle_list_registrations(journal),
        ("PUT", p) if p.starts_with("/harness/registrations/") => {
            let hash = p.trim_start_matches("/harness/registrations/");
            let endpoint = body.get("endpoint").and_then(Value::as_str).unwrap_or("");
            admin::handle_register_runtime(journal, hash, endpoint)
        }
        ("POST", "/registry/snapshots") => {
            let base = body
                .get("base_snapshot_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let bundles: Vec<String> = body
                .get("bundle_hashes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            admin::handle_compose_snapshot(journal, base, &bundles)
        }
        ("POST", "/registry/activate") => {
            let snap_id = body
                .get("snapshot_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            admin::handle_activate_snapshot(journal, snap_id, None)
        }
        ("GET", "/registry") => admin::handle_registry_info(journal),
        ("PUT", p) if p.starts_with("/grants/") => {
            let rest = p.trim_start_matches("/grants/");
            if let Some((ch, op)) = rest.split_once('/') {
                admin::handle_grant_operation(journal, ch, op)
            } else {
                Ok(json!({"ok": false, "error": "invalid_path"}))
            }
        }
        ("DELETE", p) if p.starts_with("/grants/") => {
            let rest = p.trim_start_matches("/grants/");
            if let Some((ch, op)) = rest.split_once('/') {
                admin::handle_revoke_operation(journal, ch, op)
            } else {
                Ok(json!({"ok": false, "error": "invalid_path"}))
            }
        }
        ("GET", "/grants") => {
            let ch = body.get("channel").and_then(Value::as_str);
            admin::handle_list_grants(journal, ch)
        }
        _ => Ok(json!({"ok": false, "error": "not_found"})),
    };
    match result {
        Ok(v) => write_json(stream, 200, v),
        Err(e) => {
            let msg = e.to_string();
            let status = if msg.contains("not_found") {
                404
            } else if msg.contains("conflict") {
                409
            } else if msg.contains("unauthorized") {
                401
            } else {
                400
            };
            write_json(stream, status, json!({"ok": false, "error": msg}))
        }
    }
}
