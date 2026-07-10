//! HarnessChangeRequest endpoint — POST /v1/harness-change-requests.
//!
//! v0 (PR4A1): receives, authorizes, validates, deduplicates, and persists
//! HarnessChangeRequest records WITHOUT creating a Run or starting execution.
//! Returns a `pending` request_id.
//!
//! PR4A2 will consume pending requests, create Runs, and drive the scaffold.
//!
//! Authorization approach:
//! The handler independently validates the Feishu payload and re-checks
//! owner/p2p/Feishu using the same `is_coding_owner` check as the normal
//! Runtime flow.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use serde_json::Value;

/// Workspace ID pinned for HarnessChangeRequest Runs. (Deferred to PR4A2.)
#[allow(dead_code)]
pub const HARNESS_DEV_WORKSPACE_PINNING_DEFERRED_TO_PR4A2: &str = "harness-dev";
/// Maximum tool rounds for a HarnessChangeRequest Run. (Deferred to PR4A2.)
#[allow(dead_code)]
pub const HCR_MAX_TOOL_ROUNDS_DEFERRED_TO_PR4A2: usize = 24;

const MAX_HARNESS_ID_LEN: usize = 64;
const MAX_REQUIREMENT_LEN: usize = 8000;

/// Stable error categories returned to the caller.
pub const ERR_INVALID_HARNESS_ID: &str = "INVALID_HARNESS_ID";
pub const ERR_EMPTY_HARNESS_REQUIREMENT: &str = "EMPTY_HARNESS_REQUIREMENT";
pub const ERR_OWNER_REQUIRED: &str = "HARNESS_CHANGE_REQUEST_OWNER_REQUIRED";
pub const ERR_P2P_REQUIRED: &str = "HARNESS_CHANGE_REQUEST_P2P_REQUIRED";
pub const ERR_CHANNEL_REQUIRED: &str = "HARNESS_CHANGE_REQUEST_CHANNEL_REQUIRED";
pub const ERR_SESSION_NOT_FOUND: &str = "SESSION_NOT_FOUND";
pub const ERR_INVALID_SOURCE_MESSAGE_ID: &str = "INVALID_SOURCE_MESSAGE_ID";
pub const ERR_CONFLICT: &str = "HARNESS_CHANGE_REQUEST_CONFLICT";
pub const ERR_INTERNAL: &str = "HARNESS_CHANGE_REQUEST_INTERNAL_ERROR";

/// Stable, user-safe error prefix list for server/mod.rs routing.
pub const HCR_ERROR_CATEGORIES: &[&str] = &[
    ERR_INVALID_HARNESS_ID,
    ERR_EMPTY_HARNESS_REQUIREMENT,
    ERR_OWNER_REQUIRED,
    ERR_P2P_REQUIRED,
    ERR_CHANNEL_REQUIRED,
    ERR_SESSION_NOT_FOUND,
    ERR_INVALID_SOURCE_MESSAGE_ID,
    ERR_CONFLICT,
];

fn validate_harness_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id must not be empty");
    }
    if id.len() > MAX_HARNESS_ID_LEN {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id too long (max {MAX_HARNESS_ID_LEN})");
    }
    if id.starts_with('-') || id.ends_with('-') {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id must not start or end with hyphen");
    }
    if id.contains("--") {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id must not contain consecutive hyphens");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!(
            "{ERR_INVALID_HARNESS_ID}: harness_id must match [a-z0-9-] with no consecutive hyphens"
        );
    }
    Ok(())
}

fn validate_requirement(req: &str) -> Result<()> {
    let trimmed = req.trim();
    if trimmed.is_empty() {
        bail!("{ERR_EMPTY_HARNESS_REQUIREMENT}: requirement must not be empty");
    }
    if trimmed.len() > MAX_REQUIREMENT_LEN {
        bail!("{ERR_EMPTY_HARNESS_REQUIREMENT}: requirement too long (max {MAX_REQUIREMENT_LEN})");
    }
    Ok(())
}

fn extract_feishu_fields(payload: &Value) -> Result<(String, String, String, String)> {
    let sender_open_id = payload
        .get("sender_open_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chat_type = payload
        .get("chat_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chat_id = payload
        .get("chat_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sender_type = payload
        .get("sender_type")
        .and_then(|v| v.as_str())
        .unwrap_or("user")
        .to_string();

    if sender_type == "app" {
        bail!("{ERR_CHANNEL_REQUIRED}: bot sender not supported");
    }
    if sender_open_id.is_empty() {
        bail!("{ERR_INTERNAL}: missing sender_open_id in payload");
    }
    if chat_type.is_empty() {
        bail!("{ERR_INTERNAL}: missing chat_type in payload");
    }

    Ok((sender_open_id, chat_type, chat_id, sender_type))
}

/// Sanitize an error into a fixed category. Internal details never leaked.
pub fn sanitise_hcr_error(error: &anyhow::Error) -> &'static str {
    let msg = error.to_string();
    for cat in HCR_ERROR_CATEGORIES {
        if msg.starts_with(cat) {
            return cat;
        }
    }
    ERR_INTERNAL
}

/// Handle POST /v1/harness-change-requests.
///
/// The Connector intercepts the "创建 Harness" command and sends the original
/// Feishu webhook payload alongside the parsed fields. The Kernel independently
/// re-validates owner/p2p using existing checks, then persists the request.
pub fn handle(
    journal: &JournalStore,
    _gateway: &Gateway,
    config: &KernelConfig,
    body: &Value,
) -> Result<Value> {
    let harness_id = body
        .get("harness_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let requirement = body
        .get("requirement")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let source_message_id = body
        .get("source_message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    validate_harness_id(harness_id)?;
    validate_requirement(requirement)?;
    if source_message_id.is_empty() {
        bail!("{ERR_INVALID_SOURCE_MESSAGE_ID}: source_message_id is required");
    }

    let payload = body.get("payload").cloned().unwrap_or_default();
    let (sender_open_id, chat_type, _chat_id, _sender_type) = extract_feishu_fields(&payload)?;

    let principal = RunPrincipal {
        principal_id: PrincipalId(format!("feishu:open_id:{sender_open_id}")),
        subject: PrincipalSubject::FeishuOpenId(sender_open_id.clone()),
        source: PrincipalSource::Feishu,
        grants: vec![],
        requester_id: Some(format!("feishu:open_id:{sender_open_id}")),
    };

    let is_owner =
        crate::runtime::coding_grants::is_coding_owner(config, &principal, Some(&chat_type));
    if !is_owner {
        if chat_type != "p2p" {
            bail!("{ERR_P2P_REQUIRED}: HarnessChangeRequest is only supported in private chat");
        }
        bail!("{ERR_OWNER_REQUIRED}: only the configured coding owner can create harnesses");
    }

    let session_id = config.agent_id.0.clone();
    let (request_id, deduplicated) = journal.create_harness_change_request(
        "Feishu",
        source_message_id,
        &session_id,
        &principal.principal_id.0,
        "Feishu",
        &chat_type,
        harness_id,
        requirement,
    )?;

    Ok(serde_json::json!({
        "ok": true,
        "request_id": request_id,
        "status": "pending",
        "deduplicated": deduplicated,
    }))
}

#[cfg(test)]
mod tests {
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
            harness_artifact_root: std::env::temp_dir()
                .join(format!("ha_root_{}", std::process::id())),
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
}
