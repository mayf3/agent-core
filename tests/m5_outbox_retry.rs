//! Outbox retry behavior: `retryable_failed` rows re-enter the dispatcher loop
//! once `available_at <= now`, but stay parked otherwise.

mod common;

use agent_core_kernel::adapters::InvocationAdapter;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::runtime::outbox_dispatcher::dispatch_once;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::sync::{Arc, Mutex};

struct CountingAdapter {
    calls: Arc<Mutex<Vec<InvocationId>>>,
    receipt_status: ReceiptStatus,
}

impl InvocationAdapter for CountingAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
        self.calls
            .lock()
            .unwrap()
            .push(invocation.intent().invocation_id.clone());
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status: self.receipt_status.clone(),
            external_ref: Some("test".into()),
            output: json!({"text": "ok"}),
            occurred_at: Utc::now(),
        })
    }
}

fn approved_for_run(
    gateway: &Gateway,
    run_id: &RunId,
    session_id: &SessionId,
    decision: &str,
) -> Result<ApprovedInvocation> {
    gateway.approve_invocation(
        InvocationIntent {
            invocation_id: InvocationId(format!("reply:{decision}")),
            run_id: run_id.clone(),
            operation: "stdout.send_text".to_string(),
            arguments: json!({
                "session_id": session_id.0,
                "text": "ok",
            }),
            idempotency_key: Some(format!("idem_{decision}")),
        },
        &Run {
            id: run_id.clone(),
            session_id: session_id.clone(),
            agent_id: AgentId("main".to_string()),
            trigger_event_id: EventId::new(),
            principal: common::cli_principal(),
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
                registry_snapshot_id: String::new(),
    },
        &Session {
            id: session_id.clone(),
            agent_id: AgentId("main".to_string()),
            channel: ChannelKind::Cli,
            conversation_key: "local".to_string(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        },
    )
}

fn seed_pending_outbox(journal: &JournalStore, decision: &str) -> Result<(RunId, SessionId, ApprovedInvocation, InvocationId)> {
    let run_id = RunId::new();
    let session_id = SessionId(format!("session_retry_{decision}"));
    let run = Run {
        id: run_id.clone(),
        session_id: session_id.clone(),
        agent_id: AgentId("main".to_string()),
        trigger_event_id: EventId::new(),
        principal: common::cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: String::new(),
    };
    journal.insert_run(&run)?;

    let config = common::test_config();
    let gateway = Gateway::new(config);
    let approved = approved_for_run(&gateway, &run_id, &session_id, decision)?;
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, Some(&session_id))?;
    Ok((run_id, session_id, approved, invocation_id))
}

#[test]
fn retryable_failed_with_due_available_at_is_redispatched() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (run_id, session_id, approved, invocation_id) =
        seed_pending_outbox(&journal, "retry_due")?;

    // First lease+dispatch lands in retryable_failed with available_at in the future.
    journal.lease_next_outbox_dispatch()?;
    let policy = RetryPolicy::default();
    journal.mark_outbox_retryable_failed(
        &invocation_id,
        &run_id,
        Some(&session_id),
        "transient",
        &policy,
    )?;
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::RetryableFailed)
    );

    // available_at is in the future: dispatcher must skip the row.
    let cold_calls = Arc::new(Mutex::new(vec![]));
    let cold_adapter = CountingAdapter {
        calls: cold_calls.clone(),
        receipt_status: ReceiptStatus::Succeeded,
    };
    let skipped = dispatch_once(&journal, &cold_adapter)?;
    assert!(!skipped, "retryable_failed with available_at in future must not be leased");
    assert!(cold_calls.lock().unwrap().is_empty());

    // available_at now in the past: dispatcher must lease + execute + succeed.
    journal.set_outbox_available_at_past_for_test(&invocation_id)?;
    let hot_calls = Arc::new(Mutex::new(vec![]));
    let hot_adapter = CountingAdapter {
        calls: hot_calls.clone(),
        receipt_status: ReceiptStatus::Succeeded,
    };
    let dispatched = dispatch_once(&journal, &hot_adapter)?;
    assert!(dispatched, "retryable_failed with available_at<=now must be leased");

    let pushed = hot_calls.lock().unwrap().clone();
    assert_eq!(pushed.len(), 1);
    assert_eq!(pushed[0], invocation_id);

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Succeeded)
    );

    // A second DispatchStarted event was appended by lease_next_outbox_dispatch.
    let dispatch_starts = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::DispatchStarted
                && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
        })
        .count();
    assert_eq!(dispatch_starts, 2);

    // Run was completed by the succeed path.
    assert_eq!(journal.run_status(&run_id)?.as_deref(), Some("Completed"));
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::RunCompleted
            && event.run_id.as_ref() == Some(&run_id)
    }));
    assert!(journal.verify_hash_chain()?);
    let _ = approved;
    Ok(())
}

#[test]
fn retryable_failed_with_future_available_at_is_not_redispatched() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (run_id, session_id, _approved, invocation_id) =
        seed_pending_outbox(&journal, "retry_future")?;

    journal.lease_next_outbox_dispatch()?;
    let policy = RetryPolicy::default();
    journal.mark_outbox_retryable_failed(
        &invocation_id,
        &run_id,
        Some(&session_id),
        "transient",
        &policy,
    )?;
    // available_at was just set to now + delay, definitely in the future.

    let calls = Arc::new(Mutex::new(vec![]));
    let adapter = CountingAdapter {
        calls: calls.clone(),
        receipt_status: ReceiptStatus::Succeeded,
    };
    for _ in 0..3 {
        let processed = dispatch_once(&journal, &adapter)?;
        assert!(!processed, "dispatcher must not lease a not-yet-due retryable row");
    }
    assert!(calls.lock().unwrap().is_empty(), "adapter must not be called");

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::RetryableFailed),
        "status must remain retryable_failed while not due"
    );
    let dispatch_starts = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::DispatchStarted
                && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
        })
        .count();
    assert_eq!(dispatch_starts, 1, "no new DispatchStarted while not due");
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn terminal_states_are_not_leased_by_dispatcher() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    let pending_for_state =
        |status: OutboxDispatchStatus| -> Result<(InvocationId, ApprovedInvocation)> {
            let run_id = RunId::new();
            let session_id = SessionId(format!("session_skip_{status:?}"));
            let run = Run {
                id: run_id.clone(),
                session_id: session_id.clone(),
                agent_id: AgentId("main".to_string()),
                trigger_event_id: EventId::new(),
                principal: common::cli_principal(),
                parent_run_id: None,
                delegated_by: None,
                status: RunStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                registry_snapshot_id: String::new(),
    };
            journal.insert_run(&run)?;
            let config = common::test_config();
            let gateway = Gateway::new(config);
            let approved = gateway.approve_invocation(
                InvocationIntent {
                    invocation_id: InvocationId(format!("reply:skip_{status:?}")),
                    run_id,
                    operation: "stdout.send_text".to_string(),
                    arguments: json!({"session_id": session_id.0, "text": "x"}),
                    idempotency_key: Some(format!("idem_skip_{status:?}")),
                },
                &run,
                &Session {
                    id: session_id,
                    agent_id: AgentId("main".to_string()),
                    channel: ChannelKind::Cli,
                    conversation_key: "local".to_string(),
                    summary: None,
                    summarized_until_event_id: None,
                    last_active_at: Utc::now(),
                    status: SessionStatus::Active,
                    version: 1,
                },
            )?;
            let invocation_id = approved.intent().invocation_id.clone();
            journal.queue_outbox_dispatch(&approved, None)?;
            journal.set_outbox_status_for_test(&invocation_id, status)?;
            Ok((invocation_id, approved))
        };

    let calls = Arc::new(Mutex::new(vec![]));
    let adapter = CountingAdapter {
        calls: calls.clone(),
        receipt_status: ReceiptStatus::Succeeded,
    };

    for status in [
        OutboxDispatchStatus::Failed,
        OutboxDispatchStatus::Dead,
        OutboxDispatchStatus::Dispatching,
        OutboxDispatchStatus::Unknown,
        OutboxDispatchStatus::Succeeded,
    ] {
        let (invocation_id, _approved) = pending_for_state(status)?;
        let processed = dispatch_once(&journal, &adapter)?;
        assert!(
            !processed,
            "dispatcher must not lease a row in status={status:?}"
        );
        assert_eq!(
            journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
            Some(&status),
            "status={status:?} must be unchanged after dispatch attempt"
        );
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "adapter must never be called for terminal/in-flight states"
    );
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn terminal_transition_guard_rejects_non_dispatching_state() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let run_id = RunId::new();
    let session_id = SessionId("session_guard".to_string());
    let run = Run {
        id: run_id.clone(),
        session_id: session_id.clone(),
        agent_id: AgentId("main".to_string()),
        trigger_event_id: EventId::new(),
        principal: common::cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: String::new(),
    };
    journal.insert_run(&run)?;
    let config = common::test_config();
    let gateway = Gateway::new(config);
    let approved = gateway.approve_invocation(
        InvocationIntent {
            invocation_id: InvocationId("reply:guard".to_string()),
            run_id: run_id.clone(),
            operation: "stdout.send_text".to_string(),
            arguments: json!({"session_id": session_id.0, "text": "x"}),
            idempotency_key: Some("idem_guard".to_string()),
        },
        &run,
        &Session {
            id: session_id,
            agent_id: AgentId("main".to_string()),
            channel: ChannelKind::Cli,
            conversation_key: "local".to_string(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        },
    )?;
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, None)?;
    // Row is in `pending`, not `dispatching`. Helper must reject.
    let receipt = Receipt {
        invocation_id: invocation_id.clone(),
        status: ReceiptStatus::Succeeded,
        external_ref: None,
        output: json!({}),
        occurred_at: Utc::now(),
    };
    let err = journal
        .succeed_outbox_dispatch(&receipt, &run_id, None)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("outbox_dispatch_terminal_transition_not_allowed"),
        "succeed guard must reject non-dispatching state, got: {err}"
    );
    let err = journal
        .fail_outbox_dispatch(&invocation_id, &run_id, None, "x")
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("outbox_dispatch_terminal_transition_not_allowed"));
    let err = journal
        .unknown_outbox_dispatch(&invocation_id, &run_id, None, "x")
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("outbox_dispatch_terminal_transition_not_allowed"));

    // Nothing should have been written to the journal.
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|event| matches!(
                event.kind,
                JournalEventKind::ReceiptReceived
                    | JournalEventKind::RunCompleted
                    | JournalEventKind::RunFailed
                    | JournalEventKind::OutboxDispatchUnknown
            ))
            .count(),
        0
    );
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Pending),
        "row must remain pending after rejected terminal transition"
    );
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("Running"),
        "run must remain Running after rejected terminal transition"
    );
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
