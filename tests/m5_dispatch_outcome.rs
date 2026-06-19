//! Dispatch outcome -> Run / Journal state. Exercises the dispatch_once +
//! succeed/fail/unknown helpers end-to-end with a real `runs` row.

use agent_core_kernel::adapters::InvocationAdapter;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::runtime::outbox_dispatcher::dispatch_once;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::sync::{Arc, Mutex};

mod common;

fn seed_run(journal: &JournalStore) -> Result<(RunId, SessionId)> {
    let run_id = RunId::new();
    let session_id = SessionId("session_runtime_outcome".to_string());
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
    };
    journal.insert_run(&run)?;
    Ok((run_id, session_id))
}

fn approved_for_run(
    gateway: &Gateway,
    run_id: &RunId,
    session_id: &SessionId,
    idempotency: &str,
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
            idempotency_key: Some(idempotency.to_string()),
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

struct SuccessAdapter(Arc<Mutex<Vec<InvocationId>>>);
impl InvocationAdapter for SuccessAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
        self.0
            .lock()
            .unwrap()
            .push(invocation.intent().invocation_id.clone());
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            external_ref: Some("test".into()),
            output: json!({"text": "ok"}),
            occurred_at: Utc::now(),
        })
    }
}

struct DefiniteFailAdapter;
impl InvocationAdapter for DefiniteFailAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status: ReceiptStatus::Failed,
            external_ref: None,
            output: json!({"error": "bad_request"}),
            occurred_at: Utc::now(),
        })
    }
}

struct TimeoutAdapter;
impl InvocationAdapter for TimeoutAdapter {
    fn execute(&self, _invocation: &ApprovedInvocation) -> Result<Receipt> {
        Err(anyhow::anyhow!("connection timeout"))
    }
}

#[test]
fn dispatch_success_completes_run() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config);
    let journal = JournalStore::in_memory()?;
    let (run_id, session_id) = seed_run(&journal)?;
    let adapter = SuccessAdapter(Arc::new(Mutex::new(vec![])));

    let approved = approved_for_run(
        &gateway,
        &run_id,
        &session_id,
        "idem_runtime_success",
        "runtime_success",
    )?;
    journal.queue_outbox_dispatch(&approved, Some(&session_id))?;
    assert_eq!(journal.run_status(&run_id)?.as_deref(), Some("Running"));

    dispatch_once(&journal, &adapter)?;

    assert_eq!(
        journal
            .outbox_dispatch_status(&approved.intent().invocation_id)?
            .as_ref(),
        Some(&OutboxDispatchStatus::Succeeded)
    );
    assert_eq!(journal.run_status(&run_id)?.as_deref(), Some("Completed"));
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::RunCompleted
            && event.run_id.as_ref() == Some(&run_id)
    }));
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::ReceiptReceived
            && event.correlation_id.as_deref()
                == Some(approved.intent().invocation_id.0.as_str())
    }));
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn dispatch_definite_failure_fails_run() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config);
    let journal = JournalStore::in_memory()?;
    let (run_id, session_id) = seed_run(&journal)?;
    let adapter = DefiniteFailAdapter;

    let approved = approved_for_run(
        &gateway,
        &run_id,
        &session_id,
        "idem_runtime_fail",
        "runtime_fail",
    )?;
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, Some(&session_id))?;

    dispatch_once(&journal, &adapter)?;

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Failed)
    );
    assert_eq!(journal.run_status(&run_id)?.as_deref(), Some("Failed"));
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::RunFailed
            && event.run_id.as_ref() == Some(&run_id)
    }));
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|event| event.kind == JournalEventKind::RunCompleted)
            .count(),
        0
    );
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn dispatch_unknown_does_not_complete_run() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config);
    let journal = JournalStore::in_memory()?;
    let (run_id, session_id) = seed_run(&journal)?;
    let adapter = TimeoutAdapter;

    let approved = approved_for_run(
        &gateway,
        &run_id,
        &session_id,
        "idem_runtime_unknown",
        "runtime_unknown",
    )?;
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, Some(&session_id))?;

    dispatch_once(&journal, &adapter)?;

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("Running"),
        "unknown outbox must not mutate run status"
    );
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::OutboxDispatchUnknown
            && event.run_id.as_ref() == Some(&run_id)
    }));
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|event| event.kind == JournalEventKind::RunCompleted)
            .count(),
        0
    );
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|event| event.kind == JournalEventKind::RunFailed)
            .count(),
        0
    );

    let again = dispatch_once(&journal, &adapter)?;
    assert!(!again, "unknown dispatch must not be re-leased");
    let unknown_events = journal
        .events()?
        .iter()
        .filter(|event| event.kind == JournalEventKind::OutboxDispatchUnknown)
        .count();
    assert_eq!(unknown_events, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn dispatch_success_writes_run_completed_exactly_once() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config);
    let journal = JournalStore::in_memory()?;
    let (run_id, session_id) = seed_run(&journal)?;
    let adapter = SuccessAdapter(Arc::new(Mutex::new(vec![])));

    let approved = approved_for_run(
        &gateway,
        &run_id,
        &session_id,
        "idem_runtime_once",
        "runtime_once",
    )?;
    journal.queue_outbox_dispatch(&approved, Some(&session_id))?;
    dispatch_once(&journal, &adapter)?;
    let again = dispatch_once(&journal, &adapter)?;
    assert!(!again, "succeeded dispatch must not be re-leased");

    let completed = journal
        .events()?
        .iter()
        .filter(|event| event.kind == JournalEventKind::RunCompleted)
        .count();
    assert_eq!(completed, 1);
    let receipts = journal
        .events()?
        .iter()
        .filter(|event| event.kind == JournalEventKind::ReceiptReceived)
        .count();
    assert_eq!(receipts, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
