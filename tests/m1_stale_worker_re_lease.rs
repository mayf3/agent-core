use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;

/// Phase 1: a worker job whose lease expired (worker crashed mid-job) is
/// re-leased by `lease_next_worker_job` on the next poll. This is the
/// self-heal behavior the operating guide claims. Lock it down as a
/// regression test so a future refactor cannot silently break it.
#[test]
fn stale_running_worker_job_is_re_leased_on_next_poll() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_re_lease".to_string());
    let job_id = journal.enqueue_worker_job(&event_id)?;

    // First lease: job goes running with a live lease.
    let first = journal.lease_next_worker_job()?;
    assert_eq!(first.as_ref().map(|e| &e.0), Some(&event_id.0));

    // A second lease while the first is still live returns None (still owned).
    let second = journal.lease_next_worker_job()?;
    assert!(second.is_none(), "a live lease must not be re-acquired");

    // Simulate the worker crashing mid-job: expire the lease.
    journal.expire_worker_lease_for_test(&job_id)?;

    // The next poll must re-lease the stale job (self-heal).
    let re_leased = journal.lease_next_worker_job()?;
    assert_eq!(
        re_leased.as_ref().map(|e| &e.0),
        Some(&event_id.0),
        "a stale (expired-lease) running worker job must be re-leased"
    );

    // And the stale count is now 0 again (the job has a fresh live lease).
    assert_eq!(
        journal.worker_job_stale_count()?,
        0,
        "after re-leasing, the job is no longer stale"
    );
    Ok(())
}
