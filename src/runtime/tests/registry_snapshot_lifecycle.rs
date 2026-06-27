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
use crate::llm::{
    EndpointChoice, LlmClient, LlmInput, LlmOutput, ProviderToolTurn, ToolCall,
    ToolCallResult,
};
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

fn op(name: &str, risk: Risk, description: &str, parameters: Value) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        risk,
        description: description.into(),
        parameters,
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key: format!("builtin.{name}"),
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

/// Fake LLM: captures provider_tools from each round as JSON values (Clone).
/// Uses Arc<Mutex> so data survives ownership transfer to Runtime.
struct CaptureLlm {
    /// Each entry is the full Vec<Value> of provider_tools for one LLM round.
    captured_tools: Arc<Mutex<Vec<Vec<Value>>>>,
    captured_catalogs: Arc<Mutex<Vec<String>>>,
    remaining_tool_rounds: Mutex<usize>,
}

impl CaptureLlm {
    fn new(emit_tool: bool) -> (Self, Arc<Mutex<Vec<Vec<Value>>>>, Arc<Mutex<Vec<String>>>) {
        let tools = Arc::new(Mutex::new(Vec::new()));
        let catalogs = Arc::new(Mutex::new(Vec::new()));
        let llm = Self {
            captured_tools: Arc::clone(&tools),
            captured_catalogs: Arc::clone(&catalogs),
            remaining_tool_rounds: Mutex::new(if emit_tool { 1 } else { 0 }),
        };
        (llm, tools, catalogs)
    }
}

impl LlmClient for CaptureLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        let cat = input
            .blocks
            .iter()
            .find(|b| matches!(b.kind, ContextBlockKind::ToolCatalog))
            .map(|b| b.content.clone())
            .unwrap_or_default();
        self.captured_tools.lock().unwrap().push(input.provider_tools);
        self.captured_catalogs.lock().unwrap().push(cat);
        let mut remaining = self.remaining_tool_rounds.lock().unwrap();
        if *remaining > 0 {
            *remaining -= 1;
            Ok(LlmOutput {
                provider: "test".into(),
                model: "test".into(),
                content: String::new(),
                journal_payload: json!({"status": "ok"}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "call_test".into(),
                    operation: "time.now".into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(ProviderToolTurn {
                    endpoint: EndpointChoice::Primary,
                    provider_tool_call_id: "call_test_raw".into(),
                    wire_name: "time.now".into(),
                    canonical_operation: "time.now".into(),
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "test".into(),
                content: "done".into(),
                journal_payload: json!({"status": "ok"}),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

// ========================================================================
// A  — Provider tools pinned to Run snapshot
// ========================================================================

#[test]
fn a_provider_tools_pinned_to_run_snapshot() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let config = test_config();
    let gateway = Gateway::new(config.clone());

    let snap_v1 = journal.create_registry_snapshot(v1_specs())?;
    journal.activate_registry_snapshot(&snap_v1.snapshot_id)?;

    // Run A with tool-calling LLM.
    let (llm, tools, _catalogs) = CaptureLlm::new(true);
    let runtime = Runtime::new(config.clone(), llm);
    let envelope = gateway.cli_ingress("test snapshot op".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let _run_a = runtime.deliver(&journal, &gateway, event)?;

    // Both rounds must have v1 tools.
    let t = tools.lock().unwrap();
    assert!(t.len() >= 2, "expected >=2 rounds");
    for i in 0..t.len() {
        let desc = t[i]
            .iter()
            .find(|tool| tool.pointer("/function/name").and_then(Value::as_str) == Some("time.now"))
            .and_then(|tool| tool.pointer("/function/description").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        assert_eq!(desc, "v1 description",
            "round {i}: description must be v1, got '{desc}'");
        let has_v1 = t[i].iter().any(|tool| {
            tool.pointer("/function/parameters/v1_marker").and_then(Value::as_bool) == Some(true)
        });
        assert!(has_v1, "round {i}: schema must have v1_marker");
    }
    drop(t);

    // Activate v2 — should not affect Run A's already-captured tools.
    let snap_v2 = journal.create_registry_snapshot(v2_specs())?;
    journal.activate_registry_snapshot(&snap_v2.snapshot_id)?;

    // Run B gets v2 tools.
    let (llm2, tools2, _catalogs2) = CaptureLlm::new(false);
    let runtime2 = Runtime::new(config, llm2);
    let envelope2 = gateway.cli_ingress("test op".into())?;
    let event2 = gateway.validate_ingress(&journal, envelope2)?;
    let _run_b = runtime2.deliver(&journal, &gateway, event2)?;

    let t2 = tools2.lock().unwrap();
    assert!(t2.len() >= 1, "Run B should have >=1 round");
    let b_desc = t2[0]
        .iter()
        .find(|tool| tool.pointer("/function/name").and_then(Value::as_str) == Some("time.now"))
        .and_then(|tool| tool.pointer("/function/description").and_then(Value::as_str))
        .unwrap_or("");
    assert_eq!(b_desc, "v2 description", "Run B must use v2 description");
    let has_v2 = t2[0].iter().any(|tool| {
        tool.pointer("/function/parameters/v2_marker").and_then(Value::as_bool) == Some(true)
    });
    assert!(has_v2, "Run B schema must have v2_marker");

    Ok(())
}

// ========================================================================
// B  — Context ToolCatalog pinned to Run snapshot
// ========================================================================

#[test]
fn b_context_catalog_pinned_to_run_snapshot() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let config = test_config();
    let gateway = Gateway::new(config.clone());

    let snap_v1 = journal.create_registry_snapshot(v1_specs())?;
    journal.activate_registry_snapshot(&snap_v1.snapshot_id)?;

    let (llm, _tools, catalogs) = CaptureLlm::new(true);
    let runtime = Runtime::new(config.clone(), llm);
    let envelope = gateway.cli_ingress("test op".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let _run_a = runtime.deliver(&journal, &gateway, event)?;

    let cat = catalogs.lock().unwrap();
    for i in 0..cat.len() {
    assert!(cat[i].contains("v1 description"),
        "Run A round {i}: ToolCatalog must contain v1 desc, got: {}", cat[i]);
        assert!(!cat[i].contains("v2 description"),
            "Run A round {i}: ToolCatalog must NOT contain v2 description");
    }
    drop(cat);

    // Activate v2, create Run B.
    let snap_v2 = journal.create_registry_snapshot(v2_specs())?;
    journal.activate_registry_snapshot(&snap_v2.snapshot_id)?;

    let (llm2, _tools2, catalogs2) = CaptureLlm::new(false);
    let runtime2 = Runtime::new(config, llm2);
    let envelope2 = gateway.cli_ingress("test op".into())?;
    let event2 = gateway.validate_ingress(&journal, envelope2)?;
    let _run_b = runtime2.deliver(&journal, &gateway, event2)?;

    let cat_b = catalogs2.lock().unwrap();
    assert!(cat_b.len() >= 1, "Run B should have >=1 round");
    assert!(cat_b[0].contains("v2 description"),
        "Run B: ToolCatalog must contain v2 description");
    assert!(!cat_b[0].contains("v1 description"),
        "Run B: ToolCatalog must NOT contain v1 description");

    Ok(())
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
    assert_eq!(journal.event_count()?, event_count_before + 1,
        "SessionReady event should be written before snapshot check");
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
    assert!(!err.is_empty(), "deliver must fail when snapshot is dangling");
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error must mention snapshot failure, got: {err}"
    );

    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    // SessionReady is written before the snapshot check.
    assert_eq!(journal.event_count()?, event_count_before + 1,
        "SessionReady event should be written before snapshot check");
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

        let (llm, _tools_f, _catalogs_f) = CaptureLlm::new(false);
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
        assert_eq!(run.registry_snapshot_id, v1_snapshot_id,
            "registry_snapshot_id must survive restart");

        let snap = journal.load_registry_snapshot(&run.registry_snapshot_id)?;
        assert_eq!(snap.snapshot_id, v1_snapshot_id);

        // Verify provider tools would produce v1.
        let granted: Vec<String> = run.principal.grants.iter().map(|g| g.operation.clone()).collect();
        let tools = snap.provider_tools_for_grants(&granted);
        let desc = tools
            .iter()
            .find(|t| t.pointer("/function/name").and_then(Value::as_str) == Some("time.now"))
            .and_then(|t| t.pointer("/function/description").and_then(Value::as_str))
            .unwrap_or("");
        assert_eq!(desc, "v1 description", "Provider tools must be v1 after restart");

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
        assert_eq!(spec.risk, Risk::ReadOnly,
            "Gateway must read ReadOnly from v1 snapshot");
        assert_eq!(spec.binding_key, "builtin.time_now",
            "binding_key must come from v1 snapshot");
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
