use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
use anyhow::Result;
use serde_json::json;

/// A dispatching row whose lease has expired counts as stale, and /health
/// reflects the count. This is the operator signal that distinguishes a busy
/// dispatcher from a stuck one. Phase 1 hardening.
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

    // The row is dispatching + lease expired -> stale.
    assert_eq!(journal.outbox_stale_dispatching_count()?, 1);

    // /health surfaces the stale count.
    let snapshot = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        snapshot
            .get("outbox_stale_dispatching_count")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    Ok(())
}

/// A dispatching row owned by the dispatcher loop (queued via
/// `start_outbox_dispatch`, which leaves `locked_until` NULL) does NOT count
/// as stale. Only inline-leased dispatches with an expired non-NULL lease do.
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
    // locked_until is NULL (loop-owned dispatch) -> not stale.

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

#[path = "common/mod.rs"]
mod common;
