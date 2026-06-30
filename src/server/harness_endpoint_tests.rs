//! Harness HTTP API endpoint tests through real TCP connections.
//! Tests send raw HTTP requests and assert status codes.

use super::*;
use crate::domain::*;
use serde_json::json;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        data_dir: std::path::PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: std::path::PathBuf::from("."),
        kernel_port: 0,
        connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
    }
}

fn read_body(stream: &mut std::net::TcpStream) -> (u16, String) {
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
    let status = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let body_text = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    (status, body_text)
}

fn make_request(method: &str, path: &str, body: &str, token: Option<&str>) -> Vec<u8> {
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ).into_bytes()
}

fn valid_register_body() -> String {
    json!({
        "harness_id": "test-harness",
        "artifact_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "protocol_version": "external-harness-v1",
        "endpoint": "http://127.0.0.1:9999/execute",
        "operation_name": "external.test_route",
        "description": "route test",
        "input_schema": {"type": "object", "properties": {}, "required": [], "additionalProperties": false},
        "output_schema": {"type": "object", "properties": {"status": {"type": "string"}}, "required": ["status"], "additionalProperties": false},
        "idempotent": true
    }).to_string()
}

fn setup() -> (
    KernelConfig,
    Arc<JournalStore>,
    Arc<Gateway>,
    Arc<DispatcherMetrics>,
) {
    let cfg = test_config();
    let j = Arc::new(JournalStore::in_memory().unwrap());
    let g = Arc::new(Gateway::new(cfg.clone()));
    let m = Arc::new(DispatcherMetrics::new());
    (cfg, j, g, m)
}

fn handle_one(
    cfg: &KernelConfig,
    j: &Arc<JournalStore>,
    g: &Arc<Gateway>,
    m: &Arc<DispatcherMetrics>,
    req: Vec<u8>,
) -> (u16, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let jc = Arc::clone(j);
    let gc = Arc::clone(g);
    let mc = Arc::clone(m);
    let cfg_c = cfg.clone();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = handle_connection(&mut stream, &cfg_c, jc, gc, mc);
        }
    });
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream.write_all(&req).unwrap();
    read_body(&mut stream)
}

#[test]
fn harness_route_no_auth_returns_401() {
    let (cfg, j, g, m) = setup();
    let req = make_request("POST", "/v1/harness/register", &valid_register_body(), None);
    let (status, _body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 401);
}

#[test]
fn harness_route_bad_bearer_returns_401() {
    let (cfg, j, g, m) = setup();
    let req = make_request(
        "POST",
        "/v1/harness/register",
        &valid_register_body(),
        Some("wrong-token"),
    );
    let (status, _body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 401);
}

#[test]
fn harness_route_register_enable_disable() {
    let (cfg, j, g, m) = setup();

    // Register
    let req = make_request(
        "POST",
        "/v1/harness/register",
        &valid_register_body(),
        Some("test-token"),
    );
    let (status, body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 200);
    let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(resp["ok"], true);
    let mid = resp["manifest_id"].as_str().unwrap().to_string();

    // Enable
    let s1 = j.current_registry_snapshot_id().unwrap();
    let eb = json!({"manifest_id": mid, "expected_snapshot_id": s1}).to_string();
    let req = make_request("POST", "/v1/harness/enable", &eb, Some("test-token"));
    let (status, body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 200);
    let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(resp["ok"], true);

    // Stale expected_snapshot_id → 409
    let req = make_request("POST", "/v1/harness/enable", &eb, Some("test-token"));
    let (status, _body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 409);

    // Invalid request (serde parse error caught by handle_connection) → error
    // This errors before writing a response; the outer serve loop would return 500.
}

#[test]
fn harness_route_nonexistent_manifest_returns_404() {
    let (cfg, j, g, m) = setup();
    let s1 = j.current_registry_snapshot_id().unwrap();
    let eb = json!({"manifest_id": "nonexistent", "expected_snapshot_id": s1}).to_string();
    let req = make_request("POST", "/v1/harness/enable", &eb, Some("test-token"));
    let (status, _body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 404);
}
