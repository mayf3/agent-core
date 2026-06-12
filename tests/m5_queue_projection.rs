use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn worker_job_queue_is_idempotent_and_journaled() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let source_event_id = EventId("event_source".to_string());

    let first = journal.enqueue_worker_job(&source_event_id)?;
    let second = journal.enqueue_worker_job(&source_event_id)?;

    assert_eq!(first, second);
    assert_eq!(journal.worker_job_status(&first)?.as_deref(), Some("queued"));
    let queued_events = journal
        .events()?
        .into_iter()
        .filter(|event| event.kind == JournalEventKind::WorkerJobQueued)
        .count();
    assert_eq!(queued_events, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn outbox_queue_is_idempotent_and_journaled() -> Result<()> {
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = test_session(&config);
    let run = test_run(&config, &session);
    let intent = InvocationIntent {
        invocation_id: InvocationId("reply:run_test".to_string()),
        run_id: run.id.clone(),
        operation: "stdout.send_text".to_string(),
        arguments: json!({
            "session_id": session.id.0,
            "text": "hello",
        }),
        idempotency_key: Some("stdout-reply:run_test".to_string()),
    };
    let approved = gateway.approve_invocation(intent, &run, &session)?;

    let first = journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    let second = journal.queue_outbox_dispatch(&approved, Some(&session.id))?;

    assert_eq!(first, second);
    assert_eq!(
        journal
            .outbox_dispatch_status(&InvocationId("reply:run_test".to_string()))?
            .as_deref(),
        Some("pending")
    );
    let queued_events = journal
        .events()?
        .into_iter()
        .filter(|event| event.kind == JournalEventKind::OutboxQueued)
        .count();
    assert_eq!(queued_events, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

fn test_config() -> KernelConfig {
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
    }
}

fn test_session(config: &KernelConfig) -> Session {
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

fn test_run(config: &KernelConfig, session: &Session) -> Run {
    Run {
        id: RunId("run_test".to_string()),
        session_id: session.id.clone(),
        agent_id: config.agent_id.clone(),
        trigger_event_id: EventId("event_test".to_string()),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:local".to_string()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![CapabilityGrant {
                operation: "stdout.send_text".to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("cli:local".to_string()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
