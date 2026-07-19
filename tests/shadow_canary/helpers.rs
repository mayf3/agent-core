//! Shared helpers for shadow_canary regression tests.
//!
//! Currently reserved for future use. When adding shared test
//! utilities (CaptureServer, FakeReplyAdapter wrappers, etc.),
//! place them here and remove the `#![allow(dead_code)]` below.

#![allow(dead_code)]

use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Get a unique counter value for test isolation.
pub fn next_counter() -> u64 {
    TEST_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Build a minimal KernelConfig for shadow regression tests.
pub fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-shadow-test"),
        agent_id: AgentId("shadow-test".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 4140,
        connector_execute_url: "http://127.0.0.1:4141/v1/execute".to_string(),
        ipc_token: "shadow-test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_coding_owner_id: Some("ou_shadow_owner".to_string()),
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
        harness_artifact_root: std::env::temp_dir()
            .join(format!("shadow_ha_root_{}", std::process::id())),
        max_tool_rounds: 12,
        capability_submit_token: Some("shadow-submit-token".to_string()),
        capability_decision_token: Some("shadow-decision-token".to_string()),
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: agent_core_kernel::hook::HookConfig::default(),
    }
}

/// Build a Feishu-incoming session.
pub fn feishu_session(config: &KernelConfig) -> Session {
    Session {
        id: SessionId("s_shadow_feishu".to_string()),
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Feishu,
        conversation_key: "feishu:open_id:ou_shadow_owner".to_string(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    }
}

/// Build a Run in Feishu mode with HCR binding.
pub fn feishu_run(session: &Session) -> Run {
    Run {
        id: RunId("r_shadow_hcr".to_string()),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:ou_shadow_owner".to_string()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: None,
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: "snap_init".to_string(),
        mode: RunMode::Default,
    }
}

/// Create a minimal Gateway for shadow tests.
pub fn shadow_gateway(config: &KernelConfig) -> Gateway {
    Gateway::new(config.clone())
}
