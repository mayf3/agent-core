use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
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
    assert_eq!(
        journal.worker_job_status(&first)?.as_ref(),
        Some(&WorkerJobStatus::Queued)
    );
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
fn outbox_dispatch_lifecycle_updates_projection_and_journal() -> Result<()> {
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = test_session(&config);
    let run = test_run(&config, &session);
    let approved = approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Dispatching)
    );
    journal.succeed_outbox_dispatch(
        &Receipt {
            invocation_id: invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            external_ref: Some("stdout".to_string()),
            output: json!({ "text": "hello" }),
            occurred_at: Utc::now(),
        },
        &run.id,
        Some(&session.id),
    )?;

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Succeeded)
    );
    let events = journal.events()?;
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::DispatchStarted));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::ReceiptReceived));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::RunCompleted));
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn health_reports_queue_projection_counts() -> Result<()> {
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = test_session(&config);
    let run = test_run(&config, &session);
    let approved = approved_stdout_invocation(&gateway, &run, &session)?;

    journal.enqueue_worker_job(&EventId("event_health".to_string()))?;
    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    let snapshot = health_snapshot(&journal, false, &DispatcherMetrics::new())?;

    assert_eq!(
        snapshot
            .get("worker_jobs")
            .and_then(|value| value.get("queued"))
            .and_then(|value| value.as_u64()),
        Some(1)
    );
    assert_eq!(
        snapshot
            .get("outbox_dispatches")
            .and_then(|value| value.get("pending"))
            .and_then(|value| value.as_u64()),
        Some(1)
    );
    Ok(())
}

#[test]
fn ingress_acceptance_queues_worker_job() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let event = gateway.validate_ingress(&journal, gateway.cli_ingress("queue me".to_string())?)?;
    let job_id = format!("job:deliver:{}", event.event_id.0);

    assert_eq!(
        journal.worker_job_status(&job_id)?.as_ref(),
        Some(&WorkerJobStatus::Queued)
    );
    let events = journal.events()?;
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::IngressAccepted));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::WorkerJobQueued));
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn worker_job_lifecycle_updates_projection_and_journal() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let source_event_id = EventId("event_lifecycle".to_string());
    let job_id = journal.enqueue_worker_job(&source_event_id)?;

    journal.start_worker_job(&source_event_id)?;
    assert_eq!(
        journal.worker_job_status(&job_id)?.as_ref(),
        Some(&WorkerJobStatus::Running)
    );
    journal.succeed_worker_job(&source_event_id)?;
    assert_eq!(
        journal.worker_job_status(&job_id)?.as_ref(),
        Some(&WorkerJobStatus::Succeeded)
    );
    let events = journal.events()?;
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::WorkerJobStarted));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::WorkerJobSucceeded));
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn worker_job_lease_marks_running_and_returns_event() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let source_event_id = EventId("event_lease".to_string());
    let job_id = journal.enqueue_worker_job(&source_event_id)?;

    let leased = journal.lease_next_worker_job()?;

    assert_eq!(leased, Some(source_event_id));
    assert_eq!(
        journal.worker_job_status(&job_id)?.as_ref(),
        Some(&WorkerJobStatus::Running)
    );
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::WorkerJobStarted
            && event.correlation_id.as_deref() == Some(job_id.as_str())
    }));
    Ok(())
}

#[test]
fn worker_job_lease_does_not_retake_active_running_job() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let source_event_id = EventId("event_active_lease".to_string());
    journal.enqueue_worker_job(&source_event_id)?;

    assert_eq!(journal.lease_next_worker_job()?, Some(source_event_id));
    assert_eq!(journal.lease_next_worker_job()?, None);
    let started_events = journal
        .events()?
        .into_iter()
        .filter(|event| event.kind == JournalEventKind::WorkerJobStarted)
        .count();
    assert_eq!(started_events, 1);
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
    let snap = agent_core_kernel::registry::snapshot::test_snapshot();
    let approved = gateway.approve_invocation(intent, &run, &session, &snap)?;

    let first = journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    let second = journal.queue_outbox_dispatch(&approved, Some(&session.id))?;

    assert_eq!(first, second);
    assert_eq!(
        journal
            .outbox_dispatch_status(&InvocationId("reply:run_test".to_string()))?
            .as_ref(),
        Some(&OutboxDispatchStatus::Pending)
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
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
    harness_admin_token: String::new(),
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
        registry_snapshot_id: String::new(),
    }
}

fn approved_stdout_invocation(
    gateway: &Gateway,
    run: &Run,
    session: &Session,
) -> Result<ApprovedInvocation> {
    let snap = agent_core_kernel::registry::snapshot::test_snapshot();
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
        &snap,
    )
}
