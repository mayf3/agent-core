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

/// `undelivered_ingress_count > 0` (ingress accepted but never turned into a
/// worker job / run) must degrade `/health.status`. Per
/// docs/decisions/health-rollup-undelivered-ingress.md. This is transient:
/// once the ingress is correlated to a run (as startup recovery would do by
/// re-enqueuing), status returns to ok.
#[test]
fn undelivered_ingress_degrades_health_then_recovers() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // An IngressAccepted event with no correlated run-lifecycle event leaves
    // an undelivered ingress entry.
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some("evt_undelivered"),
        json!({ "event_id": "evt_undelivered", "source": "cli" }),
    )?;
    assert_eq!(journal.undelivered_ingress_events()?.len(), 1);

    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot.get("status").and_then(|v| v.as_str()),
        Some("degraded"),
        "undelivered ingress must degrade health status"
    );

    // Simulate startup recovery correlating the ingress to a run: append a
    // RunStarted correlated to the same event id. Now undelivered is empty
    // and (with no other degraded conditions) status returns to ok.
    journal.append_event(
        JournalEventKind::RunStarted,
        None,
        None,
        Some("evt_undelivered"),
        json!({ "run_id": "run_recovered" }),
    )?;
    assert_eq!(journal.undelivered_ingress_events()?.len(), 0);
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "status must return to ok once undelivered ingress is consumed"
    );
    Ok(())
}

/// An operator can acknowledge a terminal-unknown row so it no longer degrades
/// `/health.status`. Per docs/decisions/ack-clear-terminal-unknown.md (option 1):
/// acked rows are excluded from `outbox_unknown_count` and the rollup. This
/// test uses the `ack_outbox_unknown_for_test` helper, which mirrors the
/// external ack SQL documented in the operating guide.
#[test]
fn acknowledging_terminal_unknown_clears_health_degraded() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    // Recovery reconciles the abandoned dispatch to terminal unknown.
    assert_eq!(journal.recover_unknown_invocations()?, 1);

    // Before ack: terminal-unknown degrades health.
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot.get("outbox_unknown_count").and_then(|v| v.as_u64()),
        Some(1),
        "unacked terminal-unknown is counted"
    );
    assert_eq!(
        snapshot.get("status").and_then(|v| v.as_str()),
        Some("degraded"),
        "unacked terminal-unknown degrades status"
    );

    // Operator acknowledges the terminal-unknown row.
    journal.ack_outbox_unknown_for_test(&invocation_id, true)?;

    // After ack: the row is excluded from outbox_unknown_count and status
    // returns to ok (no other degraded conditions present).
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot.get("outbox_unknown_count").and_then(|v| v.as_u64()),
        Some(0),
        "acked terminal-unknown is not counted"
    );
    assert_eq!(
        snapshot.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "acking all terminal-unknown rows clears degraded status"
    );

    // Ack is reversible: un-acking restores the degraded signal.
    journal.ack_outbox_unknown_for_test(&invocation_id, false)?;
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot.get("status").and_then(|v| v.as_str()),
        Some("degraded"),
        "un-acking a terminal-unknown row restores degraded status"
    );
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[path = "common/mod.rs"]
mod common;
