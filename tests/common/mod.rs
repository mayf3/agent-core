#![allow(dead_code)]

use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;

pub fn test_config() -> KernelConfig {
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
        // system.status is part of the dogfood agent's profile, not a
        // channel grant. It's granted here so that in-tests (which also
        // behave as a dogfood agent) can exercise the capability. See
        // KernelConfig::from_cli for the production default.
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
    }
}

pub fn test_session(config: &KernelConfig) -> Session {
    Session {
        id: SessionId("session_test".to_string()),
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Cli,
        conversation_key: "local".to_string(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    }
}

pub fn test_run(config: &KernelConfig, session: &Session) -> Run {
    Run {
        id: RunId("run_test".to_string()),
        session_id: session.id.clone(),
        agent_id: config.agent_id.clone(),
        trigger_event_id: EventId("event_test".to_string()),
        principal: cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

pub fn cli_principal() -> RunPrincipal {
    RunPrincipal {
        principal_id: PrincipalId("cli:local".to_string()),
        subject: PrincipalSubject::LocalUser,
        source: PrincipalSource::Cli,
        grants: vec![CapabilityGrant {
            operation: "stdout.send_text".to_string(),
            scope: "current_session".to_string(),
        }],
        requester_id: Some("cli:local".to_string()),
    }
}

pub fn approved_stdout_invocation(
    gateway: &Gateway,
    run: &Run,
    session: &Session,
) -> anyhow::Result<ApprovedInvocation> {
    gateway.approve_invocation(
        InvocationIntent {
            invocation_id: InvocationId("reply:run_test".to_string()),
            run_id: run.id.clone(),
            operation: "stdout.send_text".to_string(),
            arguments: json!({
                "session_id": session.id.0,
                "text": "hello",
            }),
            idempotency_key: Some("stdout-reply:run_test".to_string()),
        },
        run,
        session,
    )
}

pub fn runtime_run(run_id: &RunId, session_id: &SessionId) -> Run {
    Run {
        id: run_id.clone(),
        session_id: session_id.clone(),
        agent_id: AgentId("main".to_string()),
        trigger_event_id: EventId::new(),
        principal: cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
