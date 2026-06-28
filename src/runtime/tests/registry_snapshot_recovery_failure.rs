//! A, B, F, G — Registry snapshot lifecycle integration tests.
//!
//! A  — Provider tools are pinned to the Run's snapshot (not current/live).
//! B  — Context ToolCatalog is pinned to the Run's snapshot.
//! F  — Restart recovery preserves snapshot binding.
//! G1 — Current snapshot missing → deliver fails cleanly.
//! G2 — Current snapshot ID points to nonexistent snapshot → deliver fails cleanly.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;

// ---- Fixtures ----

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from("."),
        agent_id: AgentId("main".into()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: String::new(),
        ipc_token: "test".into(),
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
        context_max_block_chars: 4000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec!["time.now".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
    }
}

fn v1_specs() -> Vec<OperationSpec> {
    vec![
        OperationSpec {
            name: "stdout.send_text".into(),
            risk: Risk::Write,
            description: "send reply".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.stdout_send_text".into(),
        },
        OperationSpec {
            name: "time.now".into(),
            risk: Risk::ReadOnly,
            description: "v1 description".into(),
            parameters: json!({"type": "object", "v1_marker": true, "additionalProperties": false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.time_now".into(),
        },
    ]
}

#[allow(dead_code)]
fn v2_specs() -> Vec<OperationSpec> {
    vec![
        OperationSpec {
            name: "stdout.send_text".into(),
            risk: Risk::Write,
            description: "send reply".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.stdout_send_text".into(),
        },
        OperationSpec {
            name: "time.now".into(),
            risk: Risk::ReadOnly,
            description: "v2 description".into(),
            parameters: json!({"type": "object", "v2_marker": true, "additionalProperties": false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.time_now".into(),
        },
    ]
}

/// Fake LLM that counts invocations.
struct CountingLlm(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl CountingLlm {
    fn new() -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let c = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (Self(c.clone()), c)
    }
}
impl crate::llm::LlmClient for CountingLlm {
    fn complete(&self, _input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::llm::LlmOutput {
            provider: "test".into(),
            model: "test".into(),
            content: "ok".into(),
            journal_payload: serde_json::json!({}),
            tool_call: crate::llm::ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

// ========================================================================
// G1 — Current snapshot missing → deliver fails cleanly
// ========================================================================

#[test]
fn g1_current_snapshot_missing_deliver_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let err = match runtime.deliver(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!err.is_empty(), "deliver must fail when no snapshot exists");
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error must contain registry_snapshot_unavailable, got: {err}"
    );

    // SessionReady is written before the snapshot check, so event count
    // increases by 1 even on failure. Run count must not change.
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady event should be written before snapshot check"
    );
    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");

    Ok(())
}

#[test]
fn g1_current_snapshot_missing_echo_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let llm = crate::llm::LocalEchoLlm;

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let runtime = Runtime::new(config, llm);
    let err = match runtime.deliver_echo(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!err.is_empty(), "deliver_echo must fail when no snapshot");
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error must contain registry_snapshot_unavailable, got: {err}"
    );
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady event"
    );
    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");
    Ok(())
}

// ========================================================================
// G2 — Snapshot ID points to nonexistent snapshot → deliver fails cleanly
// ========================================================================

#[test]
fn g2_current_snapshot_dangling_deliver_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    journal.set_current_snapshot_id_for_test(
        "snap_nonexistent_00000000000000000000000000000000000000000000",
    );
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let err = match runtime.deliver(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(
        !err.is_empty(),
        "deliver must fail when snapshot is dangling"
    );
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error must mention snapshot failure, got: {err}"
    );

    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    // SessionReady is written before the snapshot check.
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady event should be written before snapshot check"
    );
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");

    Ok(())
}

#[test]
fn g2_current_snapshot_dangling_echo_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    journal.set_current_snapshot_id_for_test(
        "snap_nonexistent_00000000000000000000000000000000000000000000",
    );
    let config = test_config();
    let gateway = Gateway::new(config.clone());

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);
    let err = match runtime.deliver_echo(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!err.is_empty(), "deliver_echo must fail");
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error: {err}"
    );
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady"
    );
    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");
    Ok(())
}

// ========================================================================
// F  — Restart recovery preserves snapshot binding
// ========================================================================

#[test]
fn f_restart_recovery_preserves_snapshot_binding() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("reg-snap-f-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("test.sqlite");

    let run_id;
    let v1_snapshot_id;

    // Session 1: create v1 snapshot, create Run A.
    {
        let journal = JournalStore::open(&db_path)?;
        journal.initialize_registry()?;
        let config = test_config();
        let gateway = Gateway::new(config.clone());

        let snap_v1 = journal.create_registry_snapshot(v1_specs())?;
        v1_snapshot_id = snap_v1.snapshot_id.clone();
        journal.activate_registry_snapshot(&v1_snapshot_id)?;

        let (llm, _counter) = CountingLlm::new();
        let runtime = Runtime::new(config, llm);
        let envelope = gateway.cli_ingress("test".into())?;
        let event = gateway.validate_ingress(&journal, envelope)?;
        let result = runtime.deliver(&journal, &gateway, event)?;
        run_id = result.run_id;
    }

    // Session 2: reopen, verify snapshot binding.
    {
        let journal = JournalStore::open(&db_path)?;
        journal.initialize_registry()?;

        let run = journal.run(&run_id)?.expect("Run must exist after reopen");
        assert_eq!(
            run.registry_snapshot_id, v1_snapshot_id,
            "registry_snapshot_id must survive restart"
        );

        let snap = journal.load_registry_snapshot(&run.registry_snapshot_id)?;
        assert_eq!(snap.snapshot_id, v1_snapshot_id);

        // Verify provider tools would produce v1.
        let granted: Vec<String> = run
            .principal
            .grants
            .iter()
            .map(|g| g.operation.clone())
            .collect();
        let tools = snap.provider_tools_for_grants(&granted);
        let desc = tools
            .iter()
            .find(|t| t.pointer("/function/name").and_then(Value::as_str) == Some("time.now"))
            .and_then(|t| t.pointer("/function/description").and_then(Value::as_str))
            .unwrap_or("");
        assert_eq!(
            desc, "v1 description",
            "Provider tools must be v1 after restart"
        );

        // Verify Context would use v1.
        let config = test_config();
        let blocks = crate::context::ContextAssembler::from_config(&config).build(
            &journal,
            &journal.get_or_create_session(&SessionTarget {
                agent_id: config.agent_id.clone(),
                channel: ChannelKind::Cli,
                conversation_key: "local".into(),
            })?,
            &crate::domain::ValidatedEvent {
                event_id: EventId::new(),
                source: EventSource::Cli,
                principal: run.principal.clone(),
                session_target: SessionTarget {
                    agent_id: config.agent_id.clone(),
                    channel: ChannelKind::Cli,
                    conversation_key: "local".into(),
                },
                payload: RuntimeEventPayload::UserMessage {
                    text: "hi".into(),
                    message_id: None,
                    chat_id: None,
                },
                dedupe_key: "restart-test".into(),
                occurred_at: chrono::Utc::now(),
            },
            "hi",
            &granted,
            &snap,
        )?;
        let cat = blocks
            .iter()
            .find(|b| matches!(b.kind, ContextBlockKind::ToolCatalog))
            .map(|b| b.content.as_str())
            .unwrap_or("");
        assert!(cat.contains("v1 description"), "Context must be v1: {cat}");
        assert!(!cat.contains("v2 description"), "Context must NOT be v2");

        // Verify Gateway lookup uses v1.
        let spec = snap.lookup("time.now").unwrap();
        assert_eq!(
            spec.risk,
            Risk::ReadOnly,
            "Gateway must read ReadOnly from v1 snapshot"
        );
        assert_eq!(
            spec.binding_key, "builtin.time_now",
            "binding_key must come from v1 snapshot"
        );

        // === REAL GATEWAY AND DISPATCH ===
        // Use the restored Run + snapshot to validate a real tool call through
        // the production Gateway and dispatch pipeline.
        use crate::gateway::validate_tool_call;
        use crate::llm::tool_call_id_hash;

        let hashed_id = tool_call_id_hash("call_restored_time");
        let tool_call = crate::llm::ToolCall {
            id: hashed_id,
            operation: "time.now".into(),
            arguments: json!({}),
        };

        // 1. validate_tool_call — uses loaded snapshot for existence + risk.
        let intent = validate_tool_call(&tool_call, &run.id, 0, 0, &snap)
            .expect("time.now must validate against restored v1 snapshot");
        assert_eq!(
            intent.operation, "time.now",
            "Approved operation must be time.now"
        );

        // 2. Gateway::approve_invocation — uses restored Run grants + snapshot.
        let session = journal.get_or_create_session(&SessionTarget {
            agent_id: run.agent_id.clone(),
            channel: ChannelKind::Cli,
            conversation_key: run.session_id.0.clone(),
        })?;
        let approved = crate::gateway::Gateway::new(test_config()).approve_invocation(
            crate::domain::InvocationIntent {
                invocation_id: intent.invocation_id,
                run_id: run.id.clone(),
                operation: intent.operation.clone(),
                arguments: json!({"session_id": session.id.0}),
                idempotency_key: intent.idempotency_key.clone(),
            },
            &run,
            &session,
            &snap,
        )?;
        assert_eq!(
            approved.intent().operation,
            "time.now",
            "Approved operation must be time.now"
        );

        // 3+4. Production inline dispatch via dispatch_builtin_binding.
        //     This is the SINGLE authoritative binding_key → handler match
        //     that the tool loop also calls. The same code path handles
        //     ReceiptReceived journal recording.
        let correlation_id = approved.intent().invocation_id.0.clone();
        let outcome = crate::runtime::tool_execution::dispatch_builtin_binding(
            &spec,
            &approved,
            &journal,
            &run,
            &session,
            &correlation_id,
        );
        let out_text = match &outcome {
            crate::runtime::tool_loop::ToolCallOutcome::ToolResult { text } => text.clone(),
            _ => String::new(),
        };
        assert!(
            !out_text.is_empty(),
            "expected ToolResult, got non-ToolResult"
        );
        assert!(
            out_text.contains("succeeded"),
            "outcome must indicate success: {out_text}"
        );

        // Verify ReceiptReceived was written to journal for this run/invocation.
        let receipt_events: Vec<_> = journal
            .events()
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.kind == JournalEventKind::ReceiptReceived
                    && e.run_id == Some(run.id.clone())
                    && e.payload.get("invocation_id").and_then(Value::as_str)
                        == Some(&approved.intent().invocation_id.0)
            })
            .collect();
        assert_eq!(
            receipt_events.len(),
            1,
            "exactly one ReceiptReceived must exist for run_id={:?} invocation={}",
            run.id,
            approved.intent().invocation_id.0
        );
        let receipt = &receipt_events[0];
        assert_eq!(
            receipt.payload.get("status").and_then(Value::as_str),
            Some("Succeeded"),
            "Receipt must be Succeeded"
        );
        assert!(
            receipt
                .payload
                .pointer("/output/iso")
                .and_then(Value::as_str)
                .is_some(),
            "output must contain 'iso' (time.now handler)"
        );
        assert!(
            receipt
                .payload
                .pointer("/output/epoch_ms")
                .and_then(Value::as_number)
                .is_some(),
            "output must contain 'epoch_ms' (time.now handler)"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
