//! Inline tests for `delivery.rs`, kept in a separate file so that
//! `delivery.rs` stays under the 500-line structure limit.

use super::*;
use crate::domain::*;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;
struct OkAdapter;
impl InvocationAdapter for OkAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            external_ref: Some("test".into()),
            output: json!({"text": "ok"}),
            occurred_at: Utc::now(),
        })
    }
}

#[test]
fn run_dispatcher_sends_pending_outbox() -> Result<()> {
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let running = Arc::new(AtomicBool::new(true));
    let running_stop = Arc::clone(&running);

    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: InvocationId::new(),
            run_id: RunId::new(),
            operation: "stdout.send_text".into(),
            arguments: json!({"text": "hello"}),
            idempotency_key: Some("idem_dispatch".into()),
        },
        "decision_dispatch".into(),
    );
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, None)?;

    let journal_ref = Arc::clone(&journal);
    let handle = thread::spawn(move || {
        run_dispatcher(
            &journal_ref,
            &OkAdapter,
            running,
            10,
            &DispatcherMetrics::new(),
        );
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = handle.join();

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Succeeded)
    );
    assert!(journal
        .events()?
        .iter()
        .any(|e| e.kind == JournalEventKind::ReceiptReceived));
    assert!(journal
        .events()?
        .iter()
        .any(|e| e.kind == JournalEventKind::DispatchStarted));
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn run_dispatcher_skips_unknown_outbox() -> Result<()> {
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let running = Arc::new(AtomicBool::new(true));
    let running_stop = Arc::clone(&running);

    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: InvocationId::new(),
            run_id: RunId::new(),
            operation: "stdout.send_text".into(),
            arguments: json!({"text": "hello"}),
            idempotency_key: Some("idem_unknown".into()),
        },
        "decision_unknown".into(),
    );
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, None)?;
    {
        let conn = journal.conn.lock().unwrap();
        conn.execute(
            "UPDATE outbox_dispatches SET status = ?1 WHERE invocation_id = ?2",
            rusqlite::params![OutboxDispatchStatus::Unknown.as_str(), invocation_id.0],
        )?;
    }

    let journal_ref = Arc::clone(&journal);
    let handle = thread::spawn(move || {
        run_dispatcher(
            &journal_ref,
            &OkAdapter,
            running,
            10,
            &DispatcherMetrics::new(),
        );
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = handle.join();

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Unknown)
    );
    Ok(())
}

#[test]
fn run_dispatcher_stops_on_shutdown() -> Result<()> {
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let running = Arc::new(AtomicBool::new(true));
    let running_stop = Arc::clone(&running);

    let journal_ref = Arc::clone(&journal);
    let handle = thread::spawn(move || {
        run_dispatcher(
            &journal_ref,
            &OkAdapter,
            running,
            10,
            &DispatcherMetrics::new(),
        );
    });

    running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = handle.join();
    Ok(())
}

#[test]
fn run_dispatcher_updates_shared_metrics() -> Result<()> {
    // The dispatcher loop must mark the shared metrics running=true while
    // alive, record a tick timestamp on each poll, and clear running on
    // exit. This is what feeds the outbox_dispatcher_running /
    // last_dispatch_tick_at health fields (HANDOVER §4.4).
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let running = Arc::new(AtomicBool::new(true));
    let running_stop = Arc::clone(&running);
    let metrics = Arc::new(DispatcherMetrics::new());

    let journal_ref = Arc::clone(&journal);
    let metrics_ref = Arc::clone(&metrics);
    let handle = thread::spawn(move || {
        run_dispatcher(&journal_ref, &OkAdapter, running, 5, &metrics_ref);
    });

    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(
        metrics.is_running(),
        "dispatcher running flag must be set while loop is alive"
    );
    assert!(
        metrics.last_tick_at().is_some(),
        "dispatcher must record at least one tick"
    );

    running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = handle.join();

    assert!(
        !metrics.is_running(),
        "dispatcher running flag must be cleared after the loop exits"
    );
    Ok(())
}

#[test]
fn run_dispatcher_records_loop_error_category_on_failure() -> Result<()> {
    // `last_dispatch_error_category` tracks loop-level failures (when
    // dispatch_once itself returns Err — e.g. a journal/projection write
    // failure), not adapter dispatch outcomes. Adapter dispatch failures are
    // already captured per-row in outbox_dispatches.last_error via
    // unknown_outbox_dispatch, which is the dispatch-level source of truth.
    // We force a loop-level Err by dropping the outbox table so any dispatch
    // attempt fails at the journal layer.
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let running = Arc::new(AtomicBool::new(true));
    let running_stop = Arc::clone(&running);
    let metrics = Arc::new(DispatcherMetrics::new());

    {
        let conn = journal.conn.lock().unwrap();
        conn.execute_batch("DROP TABLE outbox_dispatches;")
            .expect("drop outbox table");
    }

    let journal_ref = Arc::clone(&journal);
    let metrics_ref = Arc::clone(&metrics);
    let handle = thread::spawn(move || {
        run_dispatcher(&journal_ref, &OkAdapter, running, 5, &metrics_ref);
    });

    std::thread::sleep(std::time::Duration::from_millis(80));
    running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = handle.join();

    assert!(
        metrics.last_error_category().is_some(),
        "loop-level dispatch error must populate last_dispatch_error_category"
    );
    Ok(())
}

#[test]
fn disabled_dispatcher_loop_returns_without_draining_outbox() -> Result<()> {
    use crate::config::KernelConfig;
    use std::path::PathBuf;

    let config = KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: crate::domain::AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: String::new(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
    };

    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: InvocationId::new(),
            run_id: RunId::new(),
            operation: "stdout.send_text".into(),
            arguments: json!({"text": "hello"}),
            idempotency_key: Some("idem_disabled".into()),
        },
        "decision_disabled".into(),
    );
    let invocation_id = approved.intent().invocation_id.clone();
    journal.queue_outbox_dispatch(&approved, None)?;

    let handle = start_outbox_dispatcher_loop(
        config,
        Arc::clone(&journal),
        Arc::new(AtomicBool::new(true)),
        Arc::new(DispatcherMetrics::new()),
    );
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("dispatcher thread panicked"))?;

    assert_eq!(
        journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
        Some(&OutboxDispatchStatus::Pending),
        "disabled dispatcher must not consume pending outbox"
    );
    Ok(())
}

#[test]
fn run_dispatcher_drains_multiple_pending_rows() -> Result<()> {
    use std::sync::Mutex;

    struct CountingAdapter(Arc<Mutex<Vec<InvocationId>>>);
    impl InvocationAdapter for CountingAdapter {
        fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
            self.0
                .lock()
                .unwrap()
                .push(invocation.intent().invocation_id.clone());
            Ok(Receipt {
                invocation_id: invocation.intent().invocation_id.clone(),
                status: ReceiptStatus::Succeeded,
                external_ref: Some("test".into()),
                output: json!({"text": "ok"}),
                occurred_at: Utc::now(),
            })
        }
    }

    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let running = Arc::new(AtomicBool::new(true));
    let running_stop = Arc::clone(&running);

    let calls = Arc::new(Mutex::new(vec![]));
    let mut invocation_ids = vec![];
    for idx in 0..3 {
        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId(format!("reply:multi_{idx}")),
                run_id: RunId(format!("run_multi_{idx}")),
                operation: "stdout.send_text".into(),
                arguments: json!({"text": "hello"}),
                idempotency_key: Some(format!("idem_multi_{idx}")),
            },
            format!("decision_multi_{idx}"),
        );
        invocation_ids.push(approved.intent().invocation_id.clone());
        journal.queue_outbox_dispatch(&approved, None)?;
    }

    let journal_ref = Arc::clone(&journal);
    let calls_ref = Arc::clone(&calls);
    let handle = thread::spawn(move || {
        run_dispatcher(
            &journal_ref,
            &CountingAdapter(calls_ref),
            running,
            5,
            &DispatcherMetrics::new(),
        );
    });

    // Drive a bounded number of polls: 3 dispatches + at least one extra
    // tick so the loop sees an empty queue. 200ms @ 5ms poll >= 40 ticks.
    std::thread::sleep(std::time::Duration::from_millis(200));
    running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = handle.join();

    let pushed = calls.lock().unwrap().clone();
    assert_eq!(
        pushed.len(),
        invocation_ids.len(),
        "every pending row must be dispatched exactly once"
    );
    for invocation_id in &invocation_ids {
        assert!(
            pushed.iter().any(|id| id == invocation_id),
            "invocation {invocation_id:?} must have been dispatched"
        );
        assert_eq!(
            journal.outbox_dispatch_status(invocation_id)?.as_ref(),
            Some(&OutboxDispatchStatus::Succeeded),
            "invocation {invocation_id:?} must be succeeded"
        );
        let dispatch_started = journal
            .events()?
            .iter()
            .filter(|event| {
                event.kind == JournalEventKind::DispatchStarted
                    && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
            })
            .count();
        assert_eq!(dispatch_started, 1);
        let receipt = journal
            .events()?
            .iter()
            .filter(|event| {
                event.kind == JournalEventKind::ReceiptReceived
                    && event.correlation_id.as_deref() == Some(invocation_id.0.as_str())
            })
            .count();
        assert_eq!(receipt, 1);
    }
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ---- Phase 2 M2d follow-up: periodic approval-expiry sweep ----

/// A disabled-dispatcher test config (outbox dispatcher off). Phase 2 M2d
/// follow-up tests override require_write_approval / write_approval_ttl_secs
/// on the returned clone.
fn disabled_test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: crate::domain::AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: String::new(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
    }
}

/// Build an in-memory journal with a paused (AwaitingApproval) run, for
/// sweep tests. Returns the run id and journal.
fn paused_run_for_sweep(ttl: u64) -> Result<(String, Arc<JournalStore>)> {
    use crate::gateway::Gateway;
    use crate::llm::LocalEchoLlm;
    use crate::runtime::Runtime;
    let mut config = disabled_test_config();
    config.require_write_approval = true;
    config.write_approval_ttl_secs = ttl;
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let gateway = Arc::new(Gateway::new(config.clone()));
    let runtime = Runtime::new(config, LocalEchoLlm);
    let envelope = gateway.cli_ingress("hi".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert_eq!(
        journal.run_status(&outcome.run_id)?.as_deref(),
        Some("AwaitingApproval")
    );
    Ok((outcome.run_id.0, journal))
}

#[test]
fn approval_expiry_loop_is_noop_when_ttl_zero() -> Result<()> {
    // TTL=0 → the loop thread returns immediately (does nothing). The run stays
    // AwaitingApproval. We join the handle to prove it terminated.
    let (run_id, journal) = paused_run_for_sweep(0)?;
    let running = Arc::new(AtomicBool::new(true));
    let mut config = disabled_test_config();
    config.require_write_approval = true;
    config.write_approval_ttl_secs = 0;
    let handle =
        super::start_approval_expiry_loop(config, Arc::clone(&journal), Arc::clone(&running));
    handle.join().unwrap();
    assert_eq!(
        journal.run_status(&RunId(run_id))?.as_deref(),
        Some("AwaitingApproval")
    );
    Ok(())
}

#[test]
fn approval_expiry_loop_expires_a_stale_run() -> Result<()> {
    // TTL=1, paused run. The sweep loop sleeps ~60s (min sweep), which is too
    // long for a test. Instead, drive expire_stale_approvals directly to prove
    // the *contract* the loop calls is correct under a short TTL — the loop
    // itself only adds scheduling on top. This keeps the test deterministic.
    let (run_id, journal) = paused_run_for_sweep(1)?;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let expired = journal.expire_stale_approvals(1)?;
    assert_eq!(expired, 1);
    assert_eq!(
        journal.run_status(&RunId(run_id))?.as_deref(),
        Some("Failed")
    );
    Ok(())
}
