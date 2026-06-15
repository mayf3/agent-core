//! Recovery + dispatcher health fields for the outbox dispatcher loop.

mod common;

use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::health_snapshot;
use anyhow::Result;
use serde_json::json;

#[test]
fn unknown_recovery_updates_outbox_projection_from_dispatching() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Dispatching)
    );

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );

    let restart = journal.start_outbox_dispatch(&approved, Some(&session.id));
    assert!(restart
        .unwrap_err()
        .to_string()
        .contains("outbox_dispatch_not_startable"));
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );

    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::OutboxDispatchUnknown
            && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
    }));
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|event| {
                event.kind == JournalEventKind::ReceiptReceived
                    && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
            })
            .count(),
        0
    );
    let dispatch_starts = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::DispatchStarted
                && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
        })
        .count();
    assert_eq!(dispatch_starts, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn unknown_recovery_writes_outbox_dispatch_unknown_for_journal_only_dispatch() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let run_id = RunId::new();
    let session_id = SessionId("session_unknown_outbox".to_string());
    let run = common::runtime_run(&run_id, &session_id);
    journal.insert_run(&run)?;
    journal.append_event(
        JournalEventKind::DispatchStarted,
        Some(&run_id),
        Some(&session_id),
        Some("invocation_no_projection"),
        json!({ "operation": "stdout.send_text" }),
    )?;

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);

    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::OutboxDispatchUnknown
            && event.correlation_id.as_deref() == Some("invocation_no_projection")
    }));
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|event| {
                event.kind == JournalEventKind::ReceiptReceived
                    && event.correlation_id.as_deref() == Some("invocation_no_projection")
            })
            .count(),
        0
    );
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
fn existing_outbox_dispatch_unknown_stops_scan() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let run_id = RunId::new();
    let session_id = SessionId("session_existing_unknown".to_string());
    let run = common::runtime_run(&run_id, &session_id);
    journal.insert_run(&run)?;
    journal.append_event(
        JournalEventKind::DispatchStarted,
        Some(&run_id),
        Some(&session_id),
        Some("invocation_existing_unknown"),
        json!({ "operation": "stdout.send_text" }),
    )?;
    journal.append_event(
        JournalEventKind::OutboxDispatchUnknown,
        Some(&run_id),
        Some(&session_id),
        Some("invocation_existing_unknown"),
        json!({ "error": "earlier_recovery" }),
    )?;

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 0);
    let unknown_events = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::OutboxDispatchUnknown
                && event.correlation_id.as_deref() == Some("invocation_existing_unknown")
        })
        .count();
    assert_eq!(unknown_events, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn stale_dispatching_with_existing_terminal_event_only_fixes_projection() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    journal.append_event(
        JournalEventKind::OutboxDispatchUnknown,
        Some(&run.id),
        Some(&session.id),
        Some(&invocation_id.0),
        json!({ "error": "previous_recovery_incomplete" }),
    )?;
    journal.expire_outbox_lease_for_test(&invocation_id)?;

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );
    let unknown_events = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::OutboxDispatchUnknown
                && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
        })
        .count();
    assert_eq!(unknown_events, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn stale_dispatching_unknown_never_returns_to_pending() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );
    let leased = journal.lease_next_outbox_dispatch()?;
    assert!(
        leased.is_none() || leased.unwrap().invocation_id != invocation_id,
        "unknown outbox must not be leased again"
    );
    let restart = journal.start_outbox_dispatch(&approved, Some(&session.id));
    assert!(restart.is_err());
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn health_fields_expose_dispatcher_state() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    let snapshot_disabled = health_snapshot(&journal, false)?;
    assert_eq!(
        snapshot_disabled
            .get("outbox_dispatcher_enabled")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        snapshot_disabled
            .get("outbox_pending_count")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    assert_eq!(
        snapshot_disabled
            .get("outbox_unknown_count")
            .and_then(|v| v.as_u64()),
        Some(0)
    );
    assert_eq!(
        snapshot_disabled
            .get("outbox_dispatching_count")
            .and_then(|v| v.as_u64()),
        Some(0)
    );

    let snapshot_enabled = health_snapshot(&journal, true)?;
    assert_eq!(
        snapshot_enabled
            .get("outbox_dispatcher_enabled")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    Ok(())
}
