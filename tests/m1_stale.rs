use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
use anyhow::Result;

#[path = "common/mod.rs"]
mod common;

#[test]
fn stale_running_worker_job_is_re_leased_on_next_poll() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_re_lease".to_string());
    let job_id = journal.enqueue_worker_job(&event_id)?;
    let first = journal.lease_next_worker_job()?;
    assert_eq!(first.as_ref().map(|e| &e.0), Some(&event_id.0));
    let second = journal.lease_next_worker_job()?;
    assert!(second.is_none(), "a live lease must not be re-acquired");
    journal.expire_worker_lease_for_test(&job_id)?;
    let re_leased = journal.lease_next_worker_job()?;
    assert_eq!(re_leased.as_ref().map(|e| &e.0), Some(&event_id.0));
    assert_eq!(journal.worker_job_stale_count()?, 0);
    Ok(())
}

#[test]
fn expired_dispatching_lease_is_counted_as_stale() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    journal.expire_outbox_lease_for_test(&invocation_id)?;
    assert_eq!(journal.outbox_stale_dispatching_count()?, 1);
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot
            .get("outbox_stale_dispatching_count")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    Ok(())
}

#[test]
fn null_lease_dispatching_is_not_stale() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    assert_eq!(journal.outbox_stale_dispatching_count()?, 0);
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot
            .get("outbox_stale_dispatching_count")
            .and_then(|v| v.as_u64()),
        Some(0)
    );
    Ok(())
}
