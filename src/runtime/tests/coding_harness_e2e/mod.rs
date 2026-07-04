//! Coding Harness E2E tests — full calculator development-to-42 pipeline
//! via real external harness TCP dispatch and capability proposal.
//!
//! See calculator_e2e.rs for the full proposal → approval → activation → call flow.

mod calculator_e2e;
mod helpers;

use crate::config::KernelConfig;
use std::path::PathBuf;

pub(super) fn cfg() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: crate::domain::AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
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
        harness_read_timeout_ms: 15_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ch_e2e_{}", std::process::id())),
        capability_submit_token: Some("test-submit-token".into()),
        capability_decision_token: Some("test-decision-token".into()),
    }
}
