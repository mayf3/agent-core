//! Recovery + dispatcher health fields for the outbox dispatcher loop.

mod common;

use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
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
fn stale_dispatching_with_receipt_succeeded_routes_to_succeeded() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    // Simulate the adapter returning Succeeded + the run transition landing in
    // the Journal, but the outbox projection update being lost on restart.
    journal.append_event(
        JournalEventKind::ReceiptReceived,
        Some(&run.id),
        Some(&session.id),
        Some(&invocation_id.0),
        json!({
            "status": "Succeeded",
            "external_ref": null,
            "output_kind": "text",
        }),
    )?;
    journal.expire_outbox_lease_for_test(&invocation_id)?;
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Dispatching)
    );

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Succeeded)
    );
    // No duplicate journal event: exactly one ReceiptReceived.
    let receipts = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::ReceiptReceived
                && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
        })
        .count();
    assert_eq!(receipts, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn stale_dispatching_with_receipt_failed_routes_to_failed() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    // Definite business failure landed in the Journal; projection stayed
    // dispatching across a restart.
    journal.append_event(
        JournalEventKind::ReceiptReceived,
        Some(&run.id),
        Some(&session.id),
        Some(&invocation_id.0),
        json!({
            "status": "Failed",
            "error": "connector_rejected",
            "output_kind": "error",
        }),
    )?;
    journal.expire_outbox_lease_for_test(&invocation_id)?;

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Failed)
    );
    // No duplicate journal event.
    let receipts = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::ReceiptReceived
                && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
        })
        .count();
    assert_eq!(receipts, 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn stale_dispatching_routes_by_terminal_fact_not_all_unknown() -> Result<()> {
    // Three rows, three different terminal facts in the Journal, all stale
    // dispatching. Recovery must route each projection to its matching
    // terminal state rather than collapsing all to unknown.
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);

    let cases = [
        (
            "reply:succeeded",
            JournalEventKind::ReceiptReceived,
            json!({ "status": "Succeeded", "output_kind": "text" }),
        ),
        (
            "reply:failed",
            JournalEventKind::ReceiptReceived,
            json!({ "status": "Failed", "output_kind": "error" }),
        ),
        (
            "reply:unknown",
            JournalEventKind::OutboxDispatchUnknown,
            json!({ "error": "previous_recovery_incomplete" }),
        ),
    ];
    let mut invocation_ids = Vec::new();
    let snap = agent_core_kernel::registry::snapshot::test_snapshot();
    for (suffix, _kind, _payload) in &cases {
        let approved = gateway.approve_invocation(
            InvocationIntent {
                invocation_id: InvocationId((*suffix).to_string()),
                run_id: run.id.clone(),
                operation: "stdout.send_text".to_string(),
                arguments: json!({ "session_id": session.id.0, "text": "hello" }),
                idempotency_key: Some(format!("stdout-{suffix}")),
            },
            &run,
            &session,
            &snap,
        )?;
        invocation_ids.push(approved.intent().invocation_id.clone());
        journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
        journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    }
    let succeeded_id = &invocation_ids[0];
    let failed_id = &invocation_ids[1];
    let unknown_id = &invocation_ids[2];

    for (idx, (_suffix, kind, payload)) in cases.iter().enumerate() {
        journal.append_event(
            kind.clone(),
            Some(&run.id),
            Some(&session.id),
            Some(&invocation_ids[idx].0),
            payload.clone(),
        )?;
    }
    for id in &invocation_ids {
        journal.expire_outbox_lease_for_test(id)?;
    }

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 3);
    assert_eq!(
        journal.outbox_dispatch_status(succeeded_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Succeeded)
    );
    assert_eq!(
        journal.outbox_dispatch_status(failed_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Failed)
    );
    assert_eq!(
        journal.outbox_dispatch_status(unknown_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );
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
    let snapshot_disabled = health_snapshot(&journal, false, &DispatcherMetrics::new())?;
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
    // The three observability fields (HANDOVER §4.4) are present. With a fresh
    // metrics handle the loop is not running and no tick/error is recorded.
    assert_eq!(
        snapshot_disabled
            .get("outbox_dispatcher_running")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(
        snapshot_disabled
            .get("last_dispatch_tick_at")
            .map(|v| v.is_null())
            .unwrap_or(true),
        "last_dispatch_tick_at must be null when the loop has not ticked"
    );
    assert!(
        snapshot_disabled
            .get("last_dispatch_error_category")
            .map(|v| v.is_null())
            .unwrap_or(true),
        "last_dispatch_error_category must be null when no error recorded"
    );

    let snapshot_enabled = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot_enabled
            .get("outbox_dispatcher_enabled")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    Ok(())
}

#[test]
fn health_fields_reflect_populated_dispatcher_metrics() -> Result<()> {
    // A metrics handle written to by the loop must surface its state in
    // /health: running flag, last tick timestamp, last error category.
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let _approved = common::approved_stdout_invocation(&gateway, &run, &session)?;

    let metrics = DispatcherMetrics::new();
    metrics.record_tick("2026-06-15T12:00:00Z".to_string());
    metrics.record_error_category("timeout".to_string());
    metrics.mark_started();

    let snapshot = health_snapshot(&journal, true, &metrics)?;
    assert_eq!(
        snapshot
            .get("outbox_dispatcher_running")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        snapshot
            .get("last_dispatch_tick_at")
            .and_then(|v| v.as_str()),
        Some("2026-06-15T12:00:00Z")
    );
    assert_eq!(
        snapshot
            .get("last_dispatch_error_category")
            .and_then(|v| v.as_str()),
        Some("timeout")
    );
    Ok(())
}
