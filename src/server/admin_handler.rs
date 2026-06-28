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
            admin::handle_activate_snapshot(journal, snap_id)
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
            // Safe error categories. Never leak the raw error string to the
            // client (it may contain SQL, file paths, env vars, admin tokens,
            // auth headers, etc.). All internally-generated errors MUST use one
            // of the known prefixes so that the catalog remains bounded.
            let (status, category) =
                // 404 — resource not found
                if msg.contains("not_found")
                    || msg.contains("snapshot_not_found")
                    || msg.contains("bundle_not_found")
                    || msg.contains("no such")
                {
                    (404, "not_found")
                }
                // 409 — conflict / duplicate
                else if msg.contains("bundle_conflict")
                    || msg.contains("duplicate_registration")
                    || msg.contains("UNIQUE constraint")
                {
                    (409, "conflict")
                }
                // 400 — bad request / validation error
                else if msg.contains("manifest_invalid")
                    || msg.contains("validation_failed")
                    || msg.contains("missing_field")
                    || msg.contains("invalid_path")
                    || msg.contains("unsupported_schema_keyword")
                    || msg.contains("invalid")
                    || msg.contains("malformed")
                {
                    (400, "bad_request")
                }
                else {
                    // Internal errors: never expose the raw message.
                    (500, "internal_error")
                };
            write_json(stream, status, json!({"ok": false, "error": category}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AgentId;
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn admin_config() -> KernelConfig {
        KernelConfig {
            db_path: PathBuf::from(":memory:"),
            data_dir: PathBuf::from(".agent-core-test"),
            agent_id: AgentId("main".to_string()),
            root_dir: PathBuf::from("."),
            kernel_port: 4130,
            connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
            ipc_token: "test-token".to_string(),
            feishu_allowed_open_ids: vec![],
            feishu_allowed_chat_ids: vec![],
            feishu_require_group_mention: true,
            openai_base_url: String::new(),
            openai_api_key: String::new(),
            model: String::new(),
            fallback_openai_base_url: String::new(),
            fallback_openai_api_key: String::new(),
            fallback_model: String::new(),
            model_timeout_ms: 100,
            context_recent_messages: 6,
            context_max_block_chars: 4_000,
            outbox_dispatcher_enabled: false,
            outbox_dispatcher_poll_interval_ms: 10,
            extra_allowed_operations: vec![],
            require_write_approval: false,
            write_approval_ttl_secs: 0,
            fallback_tool_name_indexed: false,
            primary_tool_name_indexed: false,
            harness_admin_token: "admin-secret".to_string(),
        }
    }

    /// Read the HTTP response body off a client stream.
    fn read_body(stream: &mut std::net::TcpStream) -> Value {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        while let Ok(n) = stream.read(&mut tmp) {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let text = String::from_utf8_lossy(&buf);
        let body_text = text.split("\r\n\r\n").nth(1).unwrap_or("").trim();
        serde_json::from_str(body_text).unwrap_or_else(|_| json!({}))
    }

    fn admin_request_status(method: &str, path: &str, token: Option<&str>, body: Value) -> u16 {
        let config = admin_config();
        let journal = Arc::new(JournalStore::in_memory().expect("in-memory"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let j = Arc::clone(&journal);
        let c = config.clone();
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let body_for_thread = body.clone();
        let token_str = token.map(|t| t.to_string());
        let method_owned = method.to_string();
        let path_owned = path.to_string();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let request = HttpRequest {
                method: method_owned,
                path: path_owned,
                bearer_token: token_str,
                body: serde_json::to_vec(&body_for_thread).unwrap(),
            };
            handle_admin_request(&mut conn, &c, &j, &request).ok();
        });
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        let auth = token.map(|t| format!("Bearer {}", t)).unwrap_or_default();
        let req_line = format!(
            "{method} {path} HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: {auth}\r\n\r\n",
            body_bytes.len(),
        );
        stream.write_all(req_line.as_bytes()).unwrap();
        stream.write_all(&body_bytes).unwrap();
        handle.join().unwrap();
        // Read response and extract HTTP status from the first line.
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        while let Ok(n) = stream.read(&mut tmp) {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let text = String::from_utf8_lossy(&buf);
        let first_line = text.lines().next().unwrap_or("HTTP/1.1 000");
        let status_str = first_line.split_whitespace().nth(1).unwrap_or("000");
        status_str.parse().unwrap_or(0)
    }

    // --- Auth tests ---

    #[test]
    fn admin_unconfigured_returns_401() {
        let config = KernelConfig {
            harness_admin_token: String::new(),
            ..admin_config()
        };
        let journal = Arc::new(JournalStore::in_memory().unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let j = Arc::clone(&journal);
        let c = config.clone();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let request = HttpRequest {
                method: "POST".to_string(),
                path: "/v1/admin/harness/bundles".to_string(),
                bearer_token: Some("admin-secret".to_string()),
                body: b"{}".to_vec(),
            };
            handle_admin_request(&mut conn, &c, &j, &request).ok();
        });
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream.write_all(b"POST /v1/admin/harness/bundles HTTP/1.1\r\nAuthorization: Bearer admin-secret\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        handle.join().unwrap();
        let resp = read_body(&mut stream);
        assert_eq!(resp.get("ok"), Some(&json!(false)));
        assert_eq!(resp.get("error"), Some(&json!("admin_not_configured")));
    }

    #[test]
    fn admin_missing_token_returns_401() {
        let status = admin_request_status("POST", "/v1/admin/harness/bundles", None, json!({}));
        assert_eq!(status, 401);
    }

    #[test]
    fn admin_wrong_token_returns_401() {
        let status = admin_request_status(
            "POST",
            "/v1/admin/harness/bundles",
            Some("wrong"),
            json!({}),
        );
        assert_eq!(status, 401);
    }

    #[test]
    fn ipc_token_not_accepted_for_admin() {
        // The admin token is "admin-secret". IPC token "test-token" should fail.
        let status = admin_request_status(
            "POST",
            "/v1/admin/harness/bundles",
            Some("test-token"),
            json!({}),
        );
        assert_eq!(status, 401);
    }

    #[test]
    fn admin_token_not_accepted_for_ingress() {
        // Admin token at regular ingress should fail.
        let config = admin_config();
        let journal = Arc::new(JournalStore::in_memory().unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let j = Arc::clone(&journal);
        let c = config.clone();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let request = HttpRequest {
                method: "POST".to_string(),
                path: "/v1/ingress".to_string(),
                bearer_token: Some("admin-secret".to_string()),
                body: serde_json::to_vec(&json!({"source": "cli_test", "payload": {"text": "hi"}}))
                    .unwrap(),
            };
            // This is admin_handler, not handle_ingress, so it would fail with routing
            // because admin_handler only handles /v1/admin/ paths.
            handle_admin_request(&mut conn, &c, &j, &request).ok();
        });
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream.write_all(b"POST /v1/ingress HTTP/1.1\r\nAuthorization: Bearer admin-secret\r\nContent-Length: 0\r\n\r\n").unwrap();
        handle.join().unwrap();
        let resp = read_body(&mut stream);
        // admin_handler doesn't handle /v1/ingress — expects not_found
        assert_eq!(resp.get("error"), Some(&json!("not_found")));
    }

    // --- Route tests ---

    #[test]
    fn admin_register_bundle_success() {
        let config = admin_config();
        let journal = Arc::new(JournalStore::in_memory().unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let j = Arc::clone(&journal);
        let c = config.clone();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let body = serde_json::json!({
                "manifest_version": "v1",
                "protocol_version": "v1",
                "bundle_id": "http_test",
                "bundle_version": "1.0",
                "operations": [{"name": "op", "description": "d", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
            });
            let request = HttpRequest {
                method: "POST".to_string(),
                path: "/v1/admin/harness/bundles".to_string(),
                bearer_token: Some("admin-secret".to_string()),
                body: serde_json::to_vec(&body).unwrap(),
            };
            handle_admin_request(&mut conn, &c, &j, &request).ok();
        });
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        let body = serde_json::json!({
            "manifest_version": "v1",
            "protocol_version": "v1",
            "bundle_id": "http_test",
            "bundle_version": "1.0",
            "operations": [{"name": "op", "description": "d", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let headers = format!(
            "POST /v1/admin/harness/bundles HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer admin-secret\r\n\r\n",
            body_bytes.len(),
        );
        stream.write_all(headers.as_bytes()).unwrap();
        stream.write_all(&body_bytes).unwrap();
        handle.join().unwrap();
        let resp = read_body(&mut stream);
        assert_eq!(resp.get("ok"), Some(&json!(true)));
        assert!(resp
            .get("bundle_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .starts_with("sha256:"));
    }

    #[test]
    fn admin_register_bundle_bad_request() {
        let status = admin_request_status(
            "POST",
            "/v1/admin/harness/bundles",
            Some("admin-secret"),
            json!({}),
        );
        assert_eq!(status, 400, "bad request should return 400");
    }

    #[test]
    fn admin_list_bundles_success() {
        let config = admin_config();
        let journal = Arc::new(JournalStore::in_memory().unwrap());
        // Register a bundle first.
        let body = serde_json::json!({
            "manifest_version": "v1",
            "protocol_version": "v1",
            "bundle_id": "listtest",
            "bundle_version": "1.0",
            "operations": [{"name": "op", "description": "d", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
        });
        crate::harness::admin::handle_register_bundle(&journal, &body).unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let j = Arc::clone(&journal);
        let c = config.clone();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let request = HttpRequest {
                method: "GET".to_string(),
                path: "/v1/admin/harness/bundles".to_string(),
                bearer_token: Some("admin-secret".to_string()),
                body: vec![],
            };
            handle_admin_request(&mut conn, &c, &j, &request).ok();
        });
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream.write_all(b"GET /v1/admin/harness/bundles HTTP/1.1\r\nAuthorization: Bearer admin-secret\r\nContent-Length: 0\r\n\r\n").unwrap();
        handle.join().unwrap();
        let resp = read_body(&mut stream);
        assert_eq!(resp.get("ok"), Some(&json!(true)));
        let bundles = resp.get("bundles").and_then(|v| v.as_array()).unwrap();
        assert!(!bundles.is_empty(), "should have at least one bundle");
    }

    #[test]
    fn admin_compose_activate_registry() {
        // HTTP routing test for compose → activate → inspect.
        // (Full handler-level tests are in crate::harness::admin::tests.)
        let status =
            admin_request_status("GET", "/v1/admin/registry", Some("admin-secret"), json!({}));
        assert_eq!(status, 200, "registry info should return 200");
    }
}
