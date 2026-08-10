//! HookBinding control-plane HTTP route tests through real TCP connections.

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
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("hb_root_{}", std::process::id())),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        force_legacy_runtime: false,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
        budget_hook: crate::hook::HookConfig::default(),
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
    )
    .into_bytes()
}

fn setup() -> (KernelConfig, Arc<JournalStore>, Arc<Gateway>, Arc<DispatcherMetrics>) {
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

fn bind_body(expected: &str) -> String {
    json!({
        "contract": "context.prepare.v0",
        "hook_id": "external:context-harness-v0",
        "hook_version": "v0",
        "binding_kind": "External",
        "binding_key": "external.context_harness",
        "provider_id": "agent-context-harness-v0",
        "endpoint": "http://127.0.0.1:7401/context",
        "expected_snapshot_id": expected,
    })
    .to_string()
}

fn unbind_body(expected: &str) -> String {
    json!({
        "contract": "context.prepare.v0",
        "expected_snapshot_id": expected,
    })
    .to_string()
}

fn current_snapshot_id(j: &JournalStore) -> String {
    j.current_registry_snapshot_id().unwrap()
}

#[test]
fn hook_binding_route_no_auth_returns_401() {
    let (cfg, j, g, m) = setup();
    let expected = current_snapshot_id(&j);
    let req = make_request("POST", "/v1/hook-bindings/bind", &bind_body(&expected), None);
    let (status, body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 401);
    assert!(body.contains("unauthorized"));
}

#[test]
fn hook_binding_route_bad_bearer_returns_401() {
    let (cfg, j, g, m) = setup();
    let expected = current_snapshot_id(&j);
    let req = make_request(
        "POST",
        "/v1/hook-bindings/bind",
        &bind_body(&expected),
        Some("wrong-token"),
    );
    let (status, _body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 401);
}

#[test]
fn hook_binding_bind_online_activates_new_snapshot() {
    let (cfg, j, g, m) = setup();
    let s0 = current_snapshot_id(&j);

    let req = make_request(
        "POST",
        "/v1/hook-bindings/bind",
        &bind_body(&s0),
        Some("test-token"),
    );
    let (status, body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 200);
    let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["changed"], true);
    assert_eq!(resp["previous_snapshot_id"], s0);
    let s1 = resp["active_snapshot_id"].as_str().unwrap().to_string();
    assert_ne!(s1, s0);
    assert_eq!(current_snapshot_id(&j), s1);

    // Audit trail.
    let events = j.events().unwrap();
    let activated = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::RegistrySnapshotActivated)
        .count();
    assert!(activated >= 1);
}

#[test]
fn hook_binding_bind_stale_expected_returns_409() {
    let (cfg, j, g, m) = setup();
    let req = make_request(
        "POST",
        "/v1/hook-bindings/bind",
        &bind_body("snap_stale_deadbeef"),
        Some("test-token"),
    );
    let (status, body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 409);
    assert!(body.contains("conflict"));
}

#[test]
fn hook_binding_bind_missing_provider_id_returns_400() {
    let (cfg, j, g, m) = setup();
    let s0 = current_snapshot_id(&j);
    let mut body = json!({
        "contract": "context.prepare.v0",
        "hook_id": "h",
        "hook_version": "v0",
        "binding_kind": "External",
        "binding_key": "k",
        "provider_id": "",
        "endpoint": "http://a",
        "expected_snapshot_id": s0,
    })
    .to_string();
    let req = make_request("POST", "/v1/hook-bindings/bind", &body, Some("test-token"));
    let (status, resp) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 400);
    assert!(resp.contains("invalid_request"));
}

#[test]
fn hook_binding_bind_missing_endpoint_returns_400() {
    let (cfg, j, g, m) = setup();
    let s0 = current_snapshot_id(&j);
    let body = json!({
        "contract": "context.prepare.v0",
        "hook_id": "h",
        "hook_version": "v0",
        "binding_kind": "External",
        "binding_key": "k",
        "provider_id": "p",
        "endpoint": "",
        "expected_snapshot_id": s0,
    })
    .to_string();
    let req = make_request("POST", "/v1/hook-bindings/bind", &body, Some("test-token"));
    let (status, _resp) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 400);
}

#[test]
fn hook_binding_unbind_online_removes_binding() {
    let (cfg, j, g, m) = setup();
    let s0 = current_snapshot_id(&j);
    // Bind first.
    let req = make_request("POST", "/v1/hook-bindings/bind", &bind_body(&s0), Some("test-token"));
    let (_status, body) = handle_one(&cfg, &j, &g, &m, req);
    let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let s1 = resp["active_snapshot_id"].as_str().unwrap().to_string();

    // Unbind.
    let req = make_request(
        "POST",
        "/v1/hook-bindings/unbind",
        &unbind_body(&s1),
        Some("test-token"),
    );
    let (status, body) = handle_one(&cfg, &j, &g, &m, req);
    assert_eq!(status, 200);
    let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(resp["ok"], true);
    let s2 = resp["active_snapshot_id"].as_str().unwrap().to_string();
    let snap = j.load_registry_snapshot(&s2).unwrap();
    assert!(snap.hook_binding("context.prepare.v0").is_none());
    assert!(snap.hook_binding("run.budget.resolve.v0").is_some());
}

#[test]
fn gateway_rejects_non_ipc_operator() {
    let (_cfg, _j, g, _m) = setup();
    let intent = crate::domain::HookBindingChangeIntent {
        action: crate::domain::HookBindingChangeAction::Bind,
        contract: "context.prepare.v0".into(),
        binding: Some(crate::registry::snapshot::HookBinding {
            contract: "context.prepare.v0".into(),
            hook_id: "h".into(),
            hook_version: "v0".into(),
            binding_kind: crate::registry::snapshot::BindingKind::External,
            binding_key: "k".into(),
            provider_id: "p".into(),
            endpoint: "http://a".into(),
        }),
        expected_snapshot_id: "s".into(),
        requested_by: "some-user".into(),
    };
    let err = g.approve_hook_binding_change(intent).unwrap_err();
    assert!(err.to_string().contains("unauthorized"));
}
