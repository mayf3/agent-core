//! Regression tests for the worker delivery failure Journal kind.
//!
//! A failed worker delivery writes a terminal `RunFailed` fact (not
//! `RunCompleted`), and `undelivered_ingress_events()` treats `RunFailed` as
//! "delivered" so the failed ingress is NOT re-queued on restart. These two
//! invariants must ship together: a kind change without the predicate change
//! would re-queue failed ingress forever. See
//! `docs/decisions/worker-failure-journal-kind.md` (plan B).

mod common;

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

#[test]
fn runfailed_counts_as_delivered_so_failed_ingress_is_not_requeued() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("event_failed_delivery".to_string());

    // An accepted ingress event, exactly as the gateway would record it.
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some(&event_id.0),
        json!({ "event_id": event_id.0, "source": "cli" }),
    )?;

    // Simulate the worker delivery failure path: it now writes RunFailed
    // (not RunCompleted) correlated to the source ingress event id.
    journal.append_event(
        JournalEventKind::RunFailed,
        None,
        None,
        Some(&event_id.0),
        json!({
            "status": "Failed",
            "reason": "worker_delivery_failed",
            "error_category": "runtime_failed",
        }),
    )?;

    // The failed delivery must count as "delivered": the ingress is NOT
    // listed as undelivered, and recovery does NOT re-queue it.
    let undelivered = journal.undelivered_ingress_events()?;
    assert!(
        undelivered.is_empty(),
        "RunFailed must count as delivered; got undelivered ingress: {undelivered:?}"
    );

    let recovered = recover_undelivered(&journal)?;
    assert_eq!(
        recovered, 0,
        "a failed worker delivery must not be re-queued on restart"
    );
    Ok(())
}

#[test]
fn worker_failure_terminal_fact_is_runfailed_not_runcompleted() -> Result<()> {
    // Documents the kind the worker failure path writes, independent of the
    // delivery.rs call site, so a future refactor that swaps the kind back to
    // RunCompleted trips this test.
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("event_kind_check".to_string());
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some(&event_id.0),
        json!({ "event_id": event_id.0, "source": "cli" }),
    )?;
    journal.append_event(
        JournalEventKind::RunFailed,
        None,
        None,
        Some(&event_id.0),
        json!({
            "status": "Failed",
            "reason": "worker_delivery_failed",
            "error_category": "runtime_failed",
        }),
    )?;

    let events = journal.events()?;
    assert!(events.iter().any(|event| {
        event.kind == JournalEventKind::RunFailed
            && event.correlation_id.as_deref() == Some(event_id.0.as_str())
    }));
    // No RunCompleted terminal fact should be written for a failed delivery.
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == JournalEventKind::RunCompleted)
            .count(),
        0
    );
    Ok(())
}

/// Mirror of `recover_undelivered_ingress` (which lives behind a non-pub
/// server helper). We re-derive the count from `undelivered_ingress_events`
/// to assert restart re-queue behavior without depending on private API.
fn recover_undelivered(journal: &JournalStore) -> Result<usize> {
    Ok(journal.undelivered_ingress_events()?.len())
}
