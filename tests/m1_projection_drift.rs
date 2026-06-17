use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
use anyhow::Result;
use serde_json::json;

/// A projection whose status matches its Journal terminal fact is NOT drift.
#[test]
fn consistent_projection_is_not_drift() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    // Journal has a terminal ReceiptReceived(Succeeded); projection still
    // dispatching (we did not call succeed_outbox_dispatch). That IS drift.
    journal.append_event(
        JournalEventKind::ReceiptReceived,
        Some(&run.id),
        Some(&session.id),
        Some(&invocation_id.0),
        json!({ "status": "Succeeded", "output_kind": "text" }),
    )?;
    // Drift: Journal says Succeeded, projection says dispatching.
    assert_eq!(
        journal.outbox_projection_drift_count()?,
        1,
        "a dispatching row whose Journal has ReceiptReceived(Succeeded) is drift"
    );

    // Simulate the projection catching up: set it to succeeded.
    journal.set_outbox_status_for_test(&invocation_id, OutboxDispatchStatus::Succeeded)?;
    assert_eq!(
        journal.outbox_projection_drift_count()?,
        0,
        "once the projection matches the Journal terminal fact, drift is 0"
    );
    Ok(())
}

/// A row whose Journal terminal fact is OutboxDispatchUnknown must have the
/// projection in `unknown` to avoid drift.
#[test]
fn unknown_terminal_fact_with_non_unknown_projection_is_drift() -> Result<()> {
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
        json!({ "error": "test" }),
    )?;
    // Projection is still dispatching; Journal says unknown -> drift.
    assert_eq!(journal.outbox_projection_drift_count()?, 1);

    // /health surfaces the drift count.
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot
            .get("outbox_projection_drift_count")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    // Per docs/decisions/health-rollup-semantics.md (档 C): projection drift
    // (projection disagrees with the Journal terminal fact) makes the Kernel's
    // state untrustworthy, so /health.status must be "degraded".
    assert_eq!(
        snapshot.get("status").and_then(|v| v.as_str()),
        Some("degraded"),
        "projection drift must degrade health status"
    );
    Ok(())
}

/// A row with no Journal terminal fact is never counted as drift (it is
/// legitimately in-flight, not inconsistent).
#[test]
fn in_flight_dispatch_without_terminal_fact_is_not_drift() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    // No terminal fact appended -> not drift (just in-flight).
    assert_eq!(journal.outbox_projection_drift_count()?, 0);
    Ok(())
}

/// After recovery reconciles an abandoned dispatch to terminal `unknown`,
/// `unknown_invocations()` is empty (no live unknowns) but
/// `outbox_unknown_count > 0`. Per docs/decisions/health-rollup-semantics.md
/// (档 C), the rollup must still report `degraded` because the dispatch
/// outcome is permanently undetermined. The pre-档 C rollup would have
/// flipped back to `ok` here.
#[test]
fn terminal_unknown_keeps_health_degraded_after_recovery() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;

    // Recovery reconciles the abandoned dispatch to terminal unknown.
    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);

    // No live unknown invocations remain (recovery wrote OutboxDispatchUnknown).
    assert_eq!(journal.unknown_invocations()?.len(), 0);
    // But the projection still carries a terminal-unknown row.
    assert_eq!(
        journal.outbox_status_count(OutboxDispatchStatus::Unknown)?,
        1
    );
    assert_eq!(journal.outbox_projection_drift_count()?, 0);

    // 档 C: terminal-unknown keeps status degraded even though
    // unknown_invocations() is empty.
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot.get("status").and_then(|v| v.as_str()),
        Some("degraded"),
        "terminal-unknown outbox rows must keep health degraded after recovery"
    );
    Ok(())
}

/// At steady state (no unknowns, no drift, hash chain intact), status is ok.
/// Guards against the rollup accidentally degrading a healthy Kernel.
#[test]
fn steady_state_health_is_ok() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(snapshot.get("status").and_then(|v| v.as_str()), Some("ok"));
    Ok(())
}

#[path = "common/mod.rs"]
mod common;
