//! Phase 2 M2d: durable approval state (opt-in).
//!
//! When an operator opts in (`require_write_approval`), a `risk: Write`
//! operation pauses the run in `AwaitingApproval` with a durable
//! `ApprovalRequested` journal fact; read-only operations still execute
//! inline. `Gateway::approve_run` resumes (queues the dispatch);
//! `Gateway::deny_run` fails the run. Default (opt-out) is byte-identical to
//! pre-M2d behavior.
//!
//! See `docs/decisions/m2d-durable-approval.md`.

mod common;

use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::LocalEchoLlm;
use agent_core_kernel::runtime::Runtime;
use anyhow::Result;
use serde_json::json;

fn config_with_approval(required: bool) -> KernelConfig {
    let mut c = common::test_config();
    c.require_write_approval = required;
    c
}

/// Count journal events of a given kind for a run.
fn count_kind(journal: &JournalStore, run_id: &RunId, kind: JournalEventKind) -> usize {
    journal
        .events()
        .unwrap()
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(run_id) && e.kind == kind)
        .count()
}

fn run_deliver_cli(required: bool) -> Result<(JournalStore, Gateway, Runtime<LocalEchoLlm, agent_core_kernel::adapters::StdoutAdapter>, RunId)> {
    let config = config_with_approval(required);
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm, agent_core_kernel::adapters::StdoutAdapter);
    let envelope = gateway.cli_ingress("hello".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    Ok((journal, gateway, runtime, outcome.run_id))
}

#[test]
fn opt_out_write_inline_approves_and_queues() -> Result<()> {
    // Default (require_write_approval=false): a Write op queues + WaitingDispatch,
    // identical to pre-M2d. Regression guard.
    let (journal, _gateway, _runtime, run_id) = run_deliver_cli(false)?;
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("WaitingDispatch")
    );
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::ApprovalRequested), 0);
    assert!(count_kind(&journal, &run_id, JournalEventKind::OutboxQueued) >= 1);
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::DispatchStarted), 0);
    Ok(())
}

#[test]
fn opt_in_write_pauses_in_awaiting_approval() -> Result<()> {
    // require_write_approval=true: a Write op pauses. ApprovalRequested is
    // journaled, run is AwaitingApproval, and it is NOT dispatched.
    let (journal, _gateway, _runtime, run_id) = run_deliver_cli(true)?;
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("AwaitingApproval")
    );
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::ApprovalRequested), 1);
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::OutboxQueued), 0);
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::DispatchStarted), 0);
    // The snapshot is retrievable.
    let snapshot = journal.approval_request_for_run(&run_id)?;
    assert!(snapshot.is_some());
    assert_eq!(
        snapshot.as_ref().unwrap().get("operation").and_then(|v| v.as_str()),
        Some("stdout.send_text")
    );
    Ok(())
}

#[test]
fn approve_run_resumes_into_waiting_dispatch() -> Result<()> {
    // A paused run, once approved, queues and advances to WaitingDispatch.
    let (journal, gateway, _runtime, run_id) = run_deliver_cli(true)?;
    gateway.approve_run(&journal, &run_id)?;
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("WaitingDispatch")
    );
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::ApprovalGranted), 1);
    assert!(count_kind(&journal, &run_id, JournalEventKind::OutboxQueued) >= 1);
    Ok(())
}

#[test]
fn deny_run_fails_the_run() -> Result<()> {
    let (journal, gateway, _runtime, run_id) = run_deliver_cli(true)?;
    gateway.deny_run(&journal, &run_id)?;
    assert_eq!(journal.run_status(&run_id)?.as_deref(), Some("Failed"));
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::ApprovalDenied), 1);
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::OutboxQueued), 0);
    Ok(())
}

#[test]
fn resume_is_idempotent_on_non_awaiting_run() -> Result<()> {
    // approve_run/deny_run on a run that is not AwaitingApproval is a no-op Ok.
    let (journal, gateway, _runtime, run_id) = run_deliver_cli(false)?; // WaitingDispatch, not awaiting
    gateway.approve_run(&journal, &run_id)?; // no-op
    gateway.deny_run(&journal, &run_id)?; // no-op
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("WaitingDispatch")
    );
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::ApprovalGranted), 0);
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::ApprovalDenied), 0);
    Ok(())
}

#[test]
fn approve_then_deny_does_not_double_transition() -> Result<()> {
    // After approve resumes the run, a subsequent deny is a no-op (run is no
    // longer AwaitingApproval).
    let (journal, gateway, _runtime, run_id) = run_deliver_cli(true)?;
    gateway.approve_run(&journal, &run_id)?;
    gateway.deny_run(&journal, &run_id)?; // no-op: already WaitingDispatch
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("WaitingDispatch")
    );
    assert_eq!(count_kind(&journal, &run_id, JournalEventKind::ApprovalDenied), 0);
    Ok(())
}

#[test]
fn approval_state_is_durable_across_reopen() -> Result<()> {
    // Build a paused run in a file-backed DB, reopen it in a fresh JournalStore,
    // and assert the pause survives (status still AwaitingApproval + the fact
    // reads back as ApprovalRequested, not Unknown).
    let dir = std::env::temp_dir().join(format!("m2d-durable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok();
    let db_path = dir.join("kernel.sqlite");

    let mut config = common::test_config();
    config.db_path = db_path.clone();
    config.require_write_approval = true;
    let run_id = {
        let journal = JournalStore::open(&db_path)?;
        let gateway = Gateway::new(config.clone());
        let runtime = Runtime::new(config, LocalEchoLlm, agent_core_kernel::adapters::StdoutAdapter);
        let envelope = gateway.cli_ingress("hi".to_string())?;
        let event = gateway.validate_ingress(&journal, envelope)?;
        let outcome = runtime.deliver(&journal, &gateway, event)?;
        outcome.run_id
    };

    // Reopen — simulates a restart. The run must still be paused.
    let journal2 = JournalStore::open(&db_path)?;
    assert_eq!(
        journal2.run_status(&run_id)?.as_deref(),
        Some("AwaitingApproval")
    );
    // The ApprovalRequested fact must read back correctly (parse_kind round-trip),
    // proving it did not degrade to the Unknown sentinel (hash-chain guard).
    let snapshot = journal2.approval_request_for_run(&run_id)?;
    assert!(snapshot.is_some());

    // And it can still be resumed after the reopen.
    let gateway2 = Gateway::new(common::test_config());
    gateway2.approve_run(&journal2, &run_id)?;
    assert_eq!(
        journal2.run_status(&run_id)?.as_deref(),
        Some("WaitingDispatch")
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn parse_kind_round_trips_approval_kinds() -> Result<()> {
    // Append an ApprovalRequested event directly, reopen, and confirm it reads
    // back as ApprovalRequested (not Unknown). Guards the hash-chain invariant
    // that tests/m5_parse_kind.rs established for unknown kinds.
    let dir = std::env::temp_dir().join(format!("m2d-parsetest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok();
    let db_path = dir.join("k.sqlite");

    {
        let journal = JournalStore::open(&db_path)?;
        journal.append_event(
            JournalEventKind::ApprovalRequested,
            None,
            None,
            None,
            json!({ "operation": "stdout.send_text" }),
        )?;
        assert!(journal.verify_hash_chain()?);
    }
    let journal2 = JournalStore::open(&db_path)?;
    let kinds: Vec<_> = journal2.events()?.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(kinds, vec![JournalEventKind::ApprovalRequested]);
    assert!(journal2.verify_hash_chain()?);

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
