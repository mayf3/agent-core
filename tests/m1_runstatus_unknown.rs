//! Regression tests for `RunStatus::Unknown`.
//!
//! When startup recovery reconciles an outbox row whose dispatch outcome is
//! unknown (a `DispatchStarted` with no terminal receipt), the owning run's
//! status must advance to `"Unknown"` so its outcome is visible on the run
//! itself, distinct from `"WaitingDispatch"` (not yet dispatched). See
//! `docs/decisions/runstatus-unknown.md`.

mod common;

use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

#[test]
fn unknown_recovery_advances_run_status_to_unknown() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let mut run = common::test_run(&config, &session);
    // Give this run a unique id + invocation so it is independent of the
    // shared test_run/approved_stdout_invocation helpers.
    run.id = RunId("run_runstatus_unknown".to_string());
    run.status = RunStatus::WaitingDispatch;
    journal.insert_run(&run)?;
    let snap = agent_core_kernel::registry::snapshot::test_snapshot();
    let approved = gateway.approve_invocation(
        InvocationIntent {
            invocation_id: InvocationId("reply:run_runstatus_unknown".to_string()),
            run_id: run.id.clone(),
            operation: "stdout.send_text".to_string(),
            arguments: json!({ "session_id": session.id.0, "text": "hello" }),
            idempotency_key: Some("stdout-reply:run_runstatus_unknown".to_string()),
        },
        &run,
        &session,
        &snap,
    )?;
    let invocation_id = approved.intent().invocation_id.clone();

    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;

    // Before recovery: run sits at WaitingDispatch; projection dispatching.
    assert_eq!(
        journal.run_status(&run.id)?.as_deref(),
        Some("WaitingDispatch")
    );
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Dispatching)
    );

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);

    // After recovery: run advanced to Unknown; outbox projection unknown.
    assert_eq!(journal.run_status(&run.id)?.as_deref(), Some("Unknown"));
    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn unknown_recovery_leaves_run_status_when_no_run_id() -> Result<()> {
    // A DispatchStarted recorded without a run_id (e.g. a journal-only
    // dispatch) cannot advance any run; recovery must not panic and must
    // still write the OutboxDispatchUnknown terminal fact. This guards the
    // `if let Some(run_id)` branch in recover_unknown_invocations.
    let journal = JournalStore::in_memory()?;
    journal.append_event(
        JournalEventKind::DispatchStarted,
        None,
        None,
        Some("invocation_no_run"),
        json!({ "operation": "stdout.send_text" }),
    )?;

    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1);

    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::OutboxDispatchUnknown
            && event.correlation_id.as_deref() == Some("invocation_no_run")
    }));
    // No run row exists, so run_status is None (recovery did not panic).
    assert_eq!(
        journal.run_status(&RunId("invocation_no_run".to_string()))?,
        None
    );
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
