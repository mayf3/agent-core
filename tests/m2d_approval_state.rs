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

fn run_deliver_cli(
    required: bool,
) -> Result<(JournalStore, Gateway, Runtime<LocalEchoLlm>, RunId)> {
    let config = config_with_approval(required);
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm);
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
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalRequested),
        0
    );
    assert!(count_kind(&journal, &run_id, JournalEventKind::OutboxQueued) >= 1);
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::DispatchStarted),
        0
    );
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
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalRequested),
        1
    );
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::OutboxQueued),
        0
    );
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::DispatchStarted),
        0
    );
    // The snapshot is retrievable.
    let snapshot = journal.approval_request_for_run(&run_id)?;
    assert!(snapshot.is_some());
    assert_eq!(
        snapshot
            .as_ref()
            .unwrap()
            .get("operation")
            .and_then(|v| v.as_str()),
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
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalGranted),
        1
    );
    assert!(count_kind(&journal, &run_id, JournalEventKind::OutboxQueued) >= 1);
    Ok(())
}

#[test]
fn deny_run_fails_the_run() -> Result<()> {
    let (journal, gateway, _runtime, run_id) = run_deliver_cli(true)?;
    gateway.deny_run(&journal, &run_id)?;
    assert_eq!(journal.run_status(&run_id)?.as_deref(), Some("Failed"));
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalDenied),
        1
    );
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::OutboxQueued),
        0
    );
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
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalGranted),
        0
    );
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalDenied),
        0
    );
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
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalDenied),
        0
    );
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
        journal.initialize_registry()?;
        let gateway = Gateway::new(config.clone());
        let runtime = Runtime::new(config, LocalEchoLlm);
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

// ---- Phase 2 M2d follow-up: approval expiry ----

struct FixedLlm;
impl agent_core_kernel::llm::LlmClient for FixedLlm {
    fn complete(
        &self,
        _input: agent_core_kernel::llm::LlmInput,
    ) -> Result<agent_core_kernel::llm::LlmOutput> {
        Ok(agent_core_kernel::llm::LlmOutput {
            provider: "local".to_string(),
            model: "fixed".to_string(),
            content: "reply".to_string(),
            journal_payload: serde_json::json!({}),
            tool_call: agent_core_kernel::llm::ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

fn config_with_ttl(ttl: u64) -> KernelConfig {
    let mut c = common::test_config();
    c.require_write_approval = true;
    c.write_approval_ttl_secs = ttl;
    c
}

/// Build a paused run; returns its id and the journal.
fn paused_run(ttl: u64) -> Result<(RunId, JournalStore)> {
    let config = config_with_ttl(ttl);
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, FixedLlm);
    let envelope = gateway.cli_ingress("hi".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert_eq!(
        journal.run_status(&outcome.run_id)?.as_deref(),
        Some("AwaitingApproval")
    );
    Ok((outcome.run_id, journal))
}

#[test]
fn ttl_zero_is_a_noop() -> Result<()> {
    // Default: a paused run stays AwaitingApproval; nothing is expired.
    let (run_id, journal) = paused_run(0)?;
    let expired = journal.expire_stale_approvals(0)?;
    assert_eq!(expired, 0);
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("AwaitingApproval")
    );
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalExpired),
        0
    );
    Ok(())
}

#[test]
fn expire_advances_stale_run_to_failed() -> Result<()> {
    // A tiny TTL means the just-paused run is already "stale" relative to it.
    let (run_id, journal) = paused_run(1)?;
    // Sleep just past the TTL so the run is older than 1 second.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let expired = journal.expire_stale_approvals(1)?;
    assert_eq!(expired, 1);
    assert_eq!(journal.run_status(&run_id)?.as_deref(), Some("Failed"));
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalExpired),
        1
    );
    // No dispatch happened (never queued).
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::OutboxQueued),
        0
    );
    Ok(())
}

#[test]
fn expire_does_not_touch_resume_or_deny_terminal_runs() -> Result<()> {
    // A run that was already resumed (WaitingDispatch) must not be expired even
    // if its ApprovalRequested is old — it's no longer AwaitingApproval.
    let (run_id, journal) = paused_run(1)?;
    let gateway = Gateway::new(common::test_config());
    gateway.approve_run(&journal, &run_id)?;
    assert_eq!(
        journal.run_status(&run_id)?.as_deref(),
        Some("WaitingDispatch")
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let expired = journal.expire_stale_approvals(1)?;
    assert_eq!(expired, 0, "a resumed run must not be expired");
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalExpired),
        0
    );
    Ok(())
}

#[test]
fn expire_is_idempotent() -> Result<()> {
    let (run_id, journal) = paused_run(1)?;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(journal.expire_stale_approvals(1)?, 1);
    // Second pass finds nothing (run is now Failed, no longer AwaitingApproval).
    assert_eq!(journal.expire_stale_approvals(1)?, 0);
    assert_eq!(
        count_kind(&journal, &run_id, JournalEventKind::ApprovalExpired),
        1
    );
    Ok(())
}

#[test]
fn parse_kind_round_trips_approval_expired() -> Result<()> {
    // Append ApprovalExpired directly, reopen, confirm it reads back correctly
    // (not Unknown) — guards the hash-chain invariant.
    let dir = std::env::temp_dir().join(format!("m2d-expiry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok();
    let db_path = dir.join("e.sqlite");
    {
        let journal = JournalStore::open(&db_path)?;
        journal.append_event(
            JournalEventKind::ApprovalExpired,
            None,
            None,
            None,
            serde_json::json!({ "operation": "stdout.send_text", "ttl_secs": 1 }),
        )?;
        assert!(journal.verify_hash_chain()?);
    }
    let journal2 = JournalStore::open(&db_path)?;
    let kinds: Vec<_> = journal2.events()?.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(kinds, vec![JournalEventKind::ApprovalExpired]);
    assert!(journal2.verify_hash_chain()?);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

// ---- Phase 2 tool-call MVP: inline system.status execution ----

#[test]
fn validate_tool_call_accepts_system_status_and_rejects_others() {
    use agent_core_kernel::domain::RunId;
    use agent_core_kernel::gateway::validate_tool_call;
    use agent_core_kernel::llm::ToolCall;
    use agent_core_kernel::registry::snapshot::test_snapshot;
    use serde_json::json;
    let snap = test_snapshot();
    let ok = validate_tool_call(
        &ToolCall {
            id: "c1".into(),
            operation: "system.status".into(),
            arguments: json!({}),
        },
        &RunId::new(),
        0,
        0,
        &snap,
    );
    assert!(ok.is_ok(), "system.status should be accepted");
    let unknown = validate_tool_call(
        &ToolCall {
            id: "c1".into(),
            operation: "shell.exec".into(),
            arguments: json!({}),
        },
        &RunId::new(),
        0,
        0,
        &snap,
    );
    assert!(unknown.is_err(), "unknown op rejected");
    let write_op = validate_tool_call(
        &ToolCall {
            id: "c1".into(),
            operation: "feishu.send_message".into(),
            arguments: json!({}),
        },
        &RunId::new(),
        0,
        0,
        &snap,
    );
    assert!(
        write_op.is_ok(),
        "Write ops are now allowed through tool-call path; Gateway approval provides security"
    );
}
