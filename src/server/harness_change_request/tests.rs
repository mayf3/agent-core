//! Tests for the POST /v1/harness-change-requests handler.
//!
//! Extracted to a separate file so `mod.rs` stays under the 500-line
//! structure limit.

use super::*;
use serde_json::{json, Value};
use std::sync::Arc;

fn hcr_config() -> KernelConfig {
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
        feishu_require_group_mention: false,
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
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        coding_harness_api_url: "http://127.0.0.1:7200".into(),
        coding_harness_artifact_digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        max_tool_rounds: 12,
        feishu_coding_owner_id: Some("ou_owner123".to_string()),
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
    }
}

fn valid_feishu_payload(overrides: Option<Value>) -> Value {
    let mut base = json!({
        "sender_open_id": "ou_owner123",
        "sender_type": "user",
        "chat_id": "oc_test_chat",
        "chat_type": "p2p",
        "message_id": "om_test_message_001",
        "message_type": "text",
        "text": "创建 Harness my-test-helper：帮我写一个代码审查助手",
    });
    if let Some(overrides) = overrides {
        if let Some(obj) = base.as_object_mut() {
            if let Some(over_obj) = overrides.as_object() {
                for (k, v) in over_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
    }
    base
}

fn setup() -> (Arc<JournalStore>, Arc<Gateway>, KernelConfig) {
    let cfg = hcr_config();
    let j = Arc::new(JournalStore::in_memory().unwrap());
    let g = Arc::new(Gateway::new(cfg.clone()));
    (j, g, cfg)
}

fn call_handler(
    j: &JournalStore,
    _g: &Gateway,
    cfg: &KernelConfig,
    hid: &str,
    req: &str,
    msg_id: &str,
    payload: Value,
) -> Value {
    let body = json!({"harness_id": hid, "requirement": req, "source_message_id": msg_id, "payload": payload});
    match handle(j, _g, cfg, &body) {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": sanitise_hcr_error(&e)}),
    }
}

fn run_count(j: &JournalStore) -> i64 {
    j.run_count().unwrap_or(0)
}
fn hcr_count(j: &JournalStore) -> i64 {
    j.harness_change_request_count().unwrap_or(0)
}
fn hcr_event_exists(j: &JournalStore) -> bool {
    j.events().ok().map_or(false, |evts| {
        evts.iter()
            .any(|e| e.kind == JournalEventKind::HarnessChangeRequested)
    })
}

#[test]
fn owner_p2p_creates_pending_harness_change_request() {
    let (j, g, cfg) = setup();
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "帮我写一个代码审查助手",
        "om_test_message_001",
        valid_feishu_payload(None),
    );
    assert_eq!(r["ok"], true);
    assert_eq!(r["status"], "pending");
    assert!(r["request_id"].as_str().unwrap_or("").starts_with("hcr_"));
    assert_eq!(r["deduplicated"], false);
}

#[test]
fn created_request_persists_harness_id_and_requirement() {
    let (j, g, cfg) = setup();
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "帮我写一个代码审查助手",
        "om_test_message_001",
        valid_feishu_payload(None),
    );
    let stored = j
        .get_harness_change_request(r["request_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored.harness_id, "my-test-helper");
    assert_eq!(stored.requirement, "帮我写一个代码审查助手");
    assert_eq!(stored.status, "pending");
    assert_eq!(stored.source, "Feishu");
    assert_eq!(stored.source_message_id, "om_test_message_001");
}

#[test]
fn request_creation_does_not_create_run() {
    let (j, g, cfg) = setup();
    assert_eq!(run_count(&j), 0);
    call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "帮我写一个代码审查助手",
        "om_test_message_001",
        valid_feishu_payload(None),
    );
    assert_eq!(run_count(&j), 0, "HCR must NOT create a Run");
}

#[test]
fn request_creation_does_not_append_run_started() {
    let (j, g, cfg) = setup();
    call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "帮我写一个代码审查助手",
        "om_test_message_001",
        valid_feishu_payload(None),
    );
    assert!(
        !j.events()
            .unwrap()
            .iter()
            .any(|e| e.kind == JournalEventKind::RunStarted),
        "HCR must NOT append RunStarted"
    );
}

#[test]
fn duplicate_source_message_returns_same_request_id() {
    let (j, g, cfg) = setup();
    let payload = valid_feishu_payload(None);
    let r1 = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "帮我写一个代码审查助手",
        "om_test_message_001",
        payload.clone(),
    );
    let r2 = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "帮我写一个代码审查助手",
        "om_test_message_001",
        payload,
    );
    assert_eq!(
        r1["request_id"], r2["request_id"],
        "same request_id on duplicate"
    );
    assert_eq!(r2["deduplicated"], true);
}

#[test]
fn duplicate_source_message_creates_one_record_only() {
    let (j, g, cfg) = setup();
    let payload = valid_feishu_payload(None);
    assert_eq!(hcr_count(&j), 0);
    call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "test",
        "om_test_msg",
        payload.clone(),
    );
    assert_eq!(hcr_count(&j), 1);
    call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "test",
        "om_test_msg",
        payload,
    );
    assert_eq!(hcr_count(&j), 1, "duplicate must not create second record");
}

#[test]
fn non_owner_denied_without_request() {
    let (j, g, cfg) = setup();
    let payload = valid_feishu_payload(Some(json!({"sender_open_id": "ou_stranger"})));
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "test",
        "om_test_msg_002",
        payload,
    );
    assert_eq!(r["error"], "HARNESS_CHANGE_REQUEST_OWNER_REQUIRED");
    assert_eq!(hcr_count(&j), 0);
    assert!(!hcr_event_exists(&j));
}

#[test]
fn group_denied_without_request() {
    let (j, g, cfg) = setup();
    let payload = valid_feishu_payload(Some(
        json!({"chat_type": "group", "chat_id": "oc_group_chat"}),
    ));
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "test",
        "om_test_msg_003",
        payload,
    );
    assert_eq!(r["error"], "HARNESS_CHANGE_REQUEST_P2P_REQUIRED");
    assert_eq!(hcr_count(&j), 0);
}

#[test]
fn invalid_harness_id_denied_without_request() {
    let (j, g, cfg) = setup();
    let payload = valid_feishu_payload(None);
    for (hid, mid) in [
        ("FOO", "010"),
        ("foo_bar", "011"),
        ("-leading", "012"),
        ("trailing-", "013"),
        ("foo--bar", "014"),
    ] {
        let r = call_handler(
            &j,
            &g,
            &cfg,
            hid,
            "test",
            &format!("om_test_msg_{mid}"),
            payload.clone(),
        );
        assert_eq!(r["error"], "INVALID_HARNESS_ID", "should reject {hid}");
        assert_eq!(hcr_count(&j), 0);
    }
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-harness-2",
        "test",
        "om_test_msg_015",
        payload,
    );
    assert_eq!(r["ok"], true);
    assert_eq!(hcr_count(&j), 1);
}

#[test]
fn empty_requirement_denied_without_request() {
    let (j, g, cfg) = setup();
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "",
        "om_test_msg_020",
        valid_feishu_payload(None),
    );
    assert_eq!(r["error"], "EMPTY_HARNESS_REQUIREMENT");
    assert_eq!(hcr_count(&j), 0);
}

#[test]
fn missing_source_message_id_denied_without_request() {
    let (j, g, cfg) = setup();
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "test",
        "",
        valid_feishu_payload(None),
    );
    assert_eq!(r["error"], "INVALID_SOURCE_MESSAGE_ID");
    assert_eq!(hcr_count(&j), 0);
}

#[test]
fn storage_failure_does_not_leave_partial_request() {
    let (j, g, cfg) = setup();
    let r = call_handler(
        &j,
        &g,
        &cfg,
        "my-test-helper",
        "帮我写一个代码审查助手",
        "om_test_msg_030",
        valid_feishu_payload(None),
    );
    assert_eq!(r["ok"], true);
    assert_eq!(hcr_count(&j), 1);
    let rid = r["request_id"].as_str().unwrap();
    let stored = j.get_harness_change_request(rid).unwrap().unwrap();
    assert_eq!(stored.status, "pending");
    assert!(hcr_event_exists(&j));
    let events = j.events().unwrap();
    let hcr_ev = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HarnessChangeRequested)
        .unwrap();
    assert_eq!(hcr_ev.payload["request_id"], rid);
    assert_eq!(hcr_ev.payload["harness_id"], "my-test-helper");
    assert_eq!(hcr_ev.payload["status"], "pending");
}
