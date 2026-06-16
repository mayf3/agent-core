use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
use anyhow::Result;

/// A running worker job whose lease expired counts as stale, and /health
/// surfaces it. Symmetric to outbox_stale_dispatching_count. Phase 1 hardening.
#[test]
fn expired_worker_lease_is_counted_as_stale() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_worker_1".to_string());
    let job_id = journal.enqueue_worker_job(&event_id)?;
    // Lease it -> status running, locked_until = now + 5min.
    let leased = journal.lease_next_worker_job()?;
    assert!(leased.is_some());

    // Before expiry: not stale.
    assert_eq!(journal.worker_job_stale_count()?, 0);

    // Expire the lease.
    journal.expire_worker_lease_for_test(&job_id)?;

    // Now it is stale.
    assert_eq!(
        journal.worker_job_stale_count()?,
        1,
        "a running worker job with an expired lease is stale"
    );

    // /health surfaces the stale count.
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot
            .get("worker_job_stale_count")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    Ok(())
}

/// A running worker job with a live lease is NOT stale.
#[test]
fn live_worker_lease_is_not_stale() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_worker_2".to_string());
    journal.enqueue_worker_job(&event_id)?;
    journal.lease_next_worker_job()?;

    assert_eq!(journal.worker_job_stale_count()?, 0);
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot
            .get("worker_job_stale_count")
            .and_then(|v| v.as_u64()),
        Some(0)
    );
    Ok(())
}

/// A queued (not yet leased) worker job is NOT stale.
#[test]
fn queued_worker_job_is_not_stale() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_worker_3".to_string());
    journal.enqueue_worker_job(&event_id)?;
    // Not leased.

    assert_eq!(journal.worker_job_stale_count()?, 0);
    Ok(())
}
