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
    EndpointChoice, LlmClient, LlmInput, LlmOutput, ProviderToolTurn, ToolCall, ToolCallResult,
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
        harness_admin_token: String::new(),
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
/// Optionally activates a snapshot after the first captured round, before
/// returning the tool call — used to activate v2 mid-Run.
/// Shared journal via Arc<JournalStore> for in-memory database access.
struct CaptureLlm {
    captured_tools: Arc<Mutex<Vec<Vec<Value>>>>,
    captured_catalogs: Arc<Mutex<Vec<String>>>,
    remaining_tool_rounds: Mutex<usize>,
    activate_snapshot_after_round1: Option<(Arc<JournalStore>, String)>,
}

impl CaptureLlm {
    fn new(
        emit_tool: bool,
        activate_snapshot_after_round1: Option<(Arc<JournalStore>, String)>,
    ) -> (Self, Arc<Mutex<Vec<Vec<Value>>>>, Arc<Mutex<Vec<String>>>) {
        let tools = Arc::new(Mutex::new(Vec::new()));
        let catalogs = Arc::new(Mutex::new(Vec::new()));
        let llm = Self {
            captured_tools: Arc::clone(&tools),
            captured_catalogs: Arc::clone(&catalogs),
            remaining_tool_rounds: Mutex::new(if emit_tool { 1 } else { 0 }),
            activate_snapshot_after_round1,
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
        self.captured_tools
            .lock()
            .unwrap()
            .push(input.provider_tools);
        self.captured_catalogs.lock().unwrap().push(cat);
        let round = self.captured_tools.lock().unwrap().len();
        // After capturing round 1 data (which is v1), activate the next
        // snapshot before returning the tool call. The follow-up round will
        // then see v2 as current, but Run A's pre-computed provider_tools/
        // Context are v1 so they won't drift.
        if round == 1 {
            if let Some((ref journal, ref snap_id)) = self.activate_snapshot_after_round1 {
                let _ = journal.activate_registry_snapshot(snap_id);
            }
        }
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
    let journal = Arc::new(journal);
    let jref: &JournalStore = &*journal;
    let config = test_config();
    let gateway = Gateway::new(config.clone());

    // Create both snapshots before Run A.
    let snap_v1 = jref.create_registry_snapshot(v1_specs())?;
    let v1_id = snap_v1.snapshot_id.clone();
    jref.activate_registry_snapshot(&v1_id)?;
    let snap_v2 = jref.create_registry_snapshot(v2_specs())?;
    let v2_id = snap_v2.snapshot_id.clone();

    let (llm, tools, _catalogs) =
        CaptureLlm::new(true, Some((Arc::clone(&journal), v2_id.clone())));
    let runtime = Runtime::new(config.clone(), llm);
    let envelope = gateway.cli_ingress("test snapshot op".into())?;
    let event = gateway.validate_ingress(jref, envelope)?;
    let run_a = runtime.deliver(jref, &gateway, event)?;

    // Both rounds must have v1 tools.
    let t = tools.lock().unwrap();
    assert!(t.len() >= 2, "expected >=2 rounds");
    for i in 0..t.len() {
        let desc = t[i]
            .iter()
            .find(|tool| tool.pointer("/function/name").and_then(Value::as_str) == Some("time.now"))
            .and_then(|tool| {
                tool.pointer("/function/description")
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .to_string();
        assert_eq!(
            desc, "v1 description",
            "round {i}: description must be v1, got '{desc}'"
        );
        let has_v1 = t[i].iter().any(|tool| {
            tool.pointer("/function/parameters/v1_marker")
                .and_then(Value::as_bool)
                == Some(true)
        });
        assert!(has_v1, "round {i}: schema must have v1_marker");
    }
    drop(t);

    // Prove both rounds belong to Run A: scan journal events for
    // LlmCompleted matching run_a.run_id. There must be ≥2 (rounds 1 and 2).
    let run_a_llm_events: Vec<_> = jref
        .events()
        .unwrap()
        .into_iter()
        .filter(|e| {
            e.kind == JournalEventKind::LlmCompleted && e.run_id == Some(run_a.run_id.clone())
        })
        .collect();
    assert!(
        run_a_llm_events.len() >= 2,
        "Run A must have >=2 LlmCompleted events (rounds 1+2), got {}",
        run_a_llm_events.len()
    );

    // v2 should now be active (activated by the CaptureLlm callback after
    // round 1 capture but before returning the tool call).
    assert_eq!(
        jref.current_registry_snapshot_id()?,
        v2_id,
        "v2 must be active after Run A's first round"
    );

    // Run B gets v2 tools.
    let (llm2, tools2, _catalogs2) = CaptureLlm::new(false, None);
    let runtime2 = Runtime::new(config, llm2);
    let envelope2 = gateway.cli_ingress("test op".into())?;
    let event2 = gateway.validate_ingress(jref, envelope2)?;
    let run_b = runtime2.deliver(jref, &gateway, event2)?;

    assert_ne!(run_a.run_id, run_b.run_id, "Run IDs must differ");

    let t2 = tools2.lock().unwrap();
    assert!(t2.len() >= 1, "Run B should have >=1 round");
    let b_desc = t2[0]
        .iter()
        .find(|tool| tool.pointer("/function/name").and_then(Value::as_str) == Some("time.now"))
        .and_then(|tool| {
            tool.pointer("/function/description")
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    assert_eq!(b_desc, "v2 description", "Run B must use v2 description");
    let has_v2 = t2[0].iter().any(|tool| {
        tool.pointer("/function/parameters/v2_marker")
            .and_then(Value::as_bool)
            == Some(true)
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
    let journal = Arc::new(journal);
    let jref: &JournalStore = &*journal;
    let config = test_config();
    let gateway = Gateway::new(config.clone());

    let snap_v1 = jref.create_registry_snapshot(v1_specs())?;
    let v1_id = snap_v1.snapshot_id.clone();
    jref.activate_registry_snapshot(&v1_id)?;
    let snap_v2 = jref.create_registry_snapshot(v2_specs())?;
    let v2_id = snap_v2.snapshot_id.clone();

    let (llm, _tools, catalogs) =
        CaptureLlm::new(true, Some((Arc::clone(&journal), v2_id.clone())));
    let runtime = Runtime::new(config.clone(), llm);
    let envelope = gateway.cli_ingress("test op".into())?;
    let event = gateway.validate_ingress(jref, envelope)?;
    let _run_a = runtime.deliver(jref, &gateway, event)?;

    let cat = catalogs.lock().unwrap();
    for i in 0..cat.len() {
        assert!(
            cat[i].contains("v1 description"),
            "Run A round {i}: ToolCatalog must contain v1 desc, got: {}",
            cat[i]
        );
        assert!(
            !cat[i].contains("v2 description"),
            "Run A round {i}: ToolCatalog must NOT contain v2 description"
        );
    }
    drop(cat);

    // v2 should now be active.
    assert_eq!(
        jref.current_registry_snapshot_id()?,
        v2_id,
        "v2 must be active after callback"
    );

    // Run B gets v2 catalog.
    let (llm2, _tools2, catalogs2) = CaptureLlm::new(false, None);
    let runtime2 = Runtime::new(config, llm2);
    let envelope2 = gateway.cli_ingress("test op".into())?;
    let event2 = gateway.validate_ingress(jref, envelope2)?;
    let _run_b = runtime2.deliver(jref, &gateway, event2)?;

    let cat_b = catalogs2.lock().unwrap();
    assert!(cat_b.len() >= 1, "Run B should have >=1 round");
    assert!(
        cat_b[0].contains("v2 description"),
        "Run B: ToolCatalog must contain v2 description"
    );
    assert!(
        !cat_b[0].contains("v1 description"),
        "Run B: ToolCatalog must NOT contain v1 description"
    );

    Ok(())
}
