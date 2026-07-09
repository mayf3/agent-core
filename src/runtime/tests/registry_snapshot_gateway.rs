//! C–E — Registry snapshot gateway integration tests.
//!
//! C — Gateway operation existence/risk comes from the Run's snapshot.
//! D — Risk classification comes from the Run's snapshot, not live/current.
//! E — Dispatch binding_key determines handler, not canonical operation name.

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
        extra_allowed_operations: vec!["system.status".to_string()],
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
        context_prepare_hook: crate::hook::HookConfig::default(),
    }
}

fn op(name: &str, risk: Risk, description: &str) -> OperationSpec {
    let binding_key = match name {
        "system.status" => "builtin.system_status".to_string(),
        "time.now" => "builtin.time_now".to_string(),
        "session.recall_recent" => "builtin.session_recall_recent".to_string(),
        _ => format!("builtin.{name}"),
    };
    OperationSpec {
        name: name.into(),
        risk,
        description: description.into(),
        parameters: json!({"type": "object", "additionalProperties": false}),
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key,
    }
}

/// Fake LLM: emits a tool call every round.
struct ToolCallLlm {
    operation: String,
}

impl ToolCallLlm {
    fn new(operation: &str) -> Self {
        Self {
            operation: operation.into(),
        }
    }
}

impl LlmClient for ToolCallLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        // emit tool call on first round only.
        let _ = input; // unused — we don't need to capture here
        Ok(LlmOutput {
            provider: "test".into(),
            model: "test".into(),
            content: String::new(),
            journal_payload: json!({"status": "ok"}),
            tool_call: ToolCallResult::Valid(ToolCall {
                id: "call_gw".into(),
                operation: self.operation.clone(),
                arguments: json!({}),
            }),
            provider_turn: Some(ProviderToolTurn {
                endpoint: EndpointChoice::Primary,
                provider_tool_call_id: "call_gw_raw".into(),
                wire_name: self.operation.clone(),
                canonical_operation: self.operation.clone(),
                reasoning_content: None,
                arguments_json: "{}".into(),
            }),
        })
    }
}

// ========================================================================
// C — Gateway operation existence from Run snapshot
// ========================================================================

#[test]
fn c_gateway_op_existence_from_run_snapshot() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // v1: no system.status.
    let v1 = vec![
        op("stdout.send_text", Risk::Write, "send reply"),
        op("time.now", Risk::ReadOnly, "time v1"),
    ];
    let s1 = journal.create_registry_snapshot(v1)?;
    journal.activate_registry_snapshot(&s1.snapshot_id)?;

    // v2: includes system.status.
    let v2 = vec![
        op("stdout.send_text", Risk::Write, "send reply"),
        op("time.now", Risk::ReadOnly, "time v2"),
        op("system.status", Risk::ReadOnly, "status"),
    ];
    let s2 = journal.create_registry_snapshot(v2)?;

    // Run A bound to v1 — system.status not in snapshot.
    journal.activate_registry_snapshot(&s1.snapshot_id)?;
    let cfg_a = test_config();
    let gw_a = Gateway::new(cfg_a.clone());
    let llm_a = ToolCallLlm::new("system.status");
    let rt_a = Runtime::new(cfg_a, llm_a);
    let ev_a = gw_a.validate_ingress(&journal, gw_a.cli_ingress("status".into())?)?;
    let run_a = rt_a.deliver(&journal, &gw_a, ev_a)?;
    let run_a_id = run_a.run_id;

    let run_a_events: Vec<_> = journal
        .events()?
        .into_iter()
        .filter(|e| e.run_id == Some(run_a_id.clone()))
        .collect();
    let status_rejected = run_a_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ToolCallRejected
                && e.payload.get("error_category").and_then(Value::as_str)
                    == Some("unknown_operation")
        })
        .count();
    assert!(
        status_rejected > 0,
        "Run A: system.status must be rejected (unknown)"
    );
    let run_a_succeeded = run_a_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
        })
        .count();
    assert_eq!(
        run_a_succeeded, 0,
        "Run A must have 0 Succeeded Receipts (system.status not in v1)"
    );

    // Run B bound to v2 — system.status exists.
    journal.activate_registry_snapshot(&s2.snapshot_id)?;
    let cfg_b = test_config();
    let gw_b = Gateway::new(cfg_b.clone());
    let llm_b = ToolCallLlm::new("system.status");
    let rt_b = Runtime::new(cfg_b, llm_b);
    let ev_b = gw_b.validate_ingress(&journal, gw_b.cli_ingress("status".into())?)?;
    let run_b = rt_b.deliver(&journal, &gw_b, ev_b)?;
    let run_b_id = run_b.run_id;

    let run_b_events: Vec<_> = journal
        .events()?
        .into_iter()
        .filter(|e| e.run_id == Some(run_b_id.clone()))
        .collect();
    let run_b_unknown = run_b_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ToolCallRejected
                && e.payload.get("error_category").and_then(Value::as_str)
                    == Some("unknown_operation")
        })
        .count();
    assert_eq!(
        run_b_unknown, 0,
        "Run B must have 0 unknown_operation rejections"
    );
    let run_b_succeeded = run_b_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
        })
        .count();
    assert!(
        run_b_succeeded > 0,
        "Run B must have Succeeded Receipts (system.status in v2)"
    );

    Ok(())
}

// ========================================================================
// D — Risk from Run snapshot
// ========================================================================

#[test]
fn d_risk_from_run_snapshot() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // v1: system.status is ReadOnly.
    let v1 = vec![
        op("stdout.send_text", Risk::Write, "send reply"),
        op("system.status", Risk::ReadOnly, "ro in v1"),
    ];
    let s1 = journal.create_registry_snapshot(v1)?;
    journal.activate_registry_snapshot(&s1.snapshot_id)?;

    // v2: system.status is Write (not yet activated).
    let v2 = vec![
        op("stdout.send_text", Risk::Write, "send reply"),
        op("system.status", Risk::Write, "write in v2"),
    ];
    let s2 = journal.create_registry_snapshot(v2)?;

    // Run A bound to v1 (ReadOnly) — tool call for system.status succeeds.
    let cfg_a = test_config();
    let gw_a = Gateway::new(cfg_a.clone());
    let llm_a = ToolCallLlm::new("system.status");
    let rt_a = Runtime::new(cfg_a, llm_a);
    let ev_a = gw_a.validate_ingress(&journal, gw_a.cli_ingress("op".into())?)?;
    let run_a = rt_a.deliver(&journal, &gw_a, ev_a)?;

    let a_events: Vec<_> = journal
        .events()?
        .into_iter()
        .filter(|e| e.run_id == Some(run_a.run_id.clone()))
        .collect();
    let a_not_allowed = a_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ToolCallRejected
                && e.payload.get("error_category").and_then(Value::as_str)
                    == Some("operation_not_allowed")
        })
        .count();
    assert_eq!(
        a_not_allowed, 0,
        "Run A (v1 ReadOnly) must have 0 operation_not_allowed"
    );
    let a_succeeded = a_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
        })
        .count();
    assert!(
        a_succeeded > 0,
        "Run A (v1 ReadOnly) must have Succeeded Receipts"
    );

    // Activate v2 (Write) for Run B.
    journal.activate_registry_snapshot(&s2.snapshot_id)?;

    // Run B bound to v2 (Write) — tool call rejected as operation_not_allowed.
    let cfg_b = test_config();
    let gw_b = Gateway::new(cfg_b.clone());
    let llm_b = ToolCallLlm::new("system.status");
    let rt_b = Runtime::new(cfg_b, llm_b);
    let ev_b = gw_b.validate_ingress(&journal, gw_b.cli_ingress("op".into())?)?;
    let run_b = rt_b.deliver(&journal, &gw_b, ev_b)?;

    let b_events: Vec<_> = journal
        .events()?
        .into_iter()
        .filter(|e| e.run_id == Some(run_b.run_id.clone()))
        .collect();
    let b_not_allowed = b_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ToolCallRejected
                && e.payload.get("error_category").and_then(Value::as_str)
                    == Some("operation_not_allowed")
        })
        .count();
    assert!(
        b_not_allowed > 0,
        "Run B (v2 Write) must be rejected as operation_not_allowed"
    );
    let b_succeeded = b_events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
        })
        .count();
    assert_eq!(
        b_succeeded, 0,
        "Run B (v2 Write) must have 0 Succeeded Receipts"
    );

    Ok(())
}

// ========================================================================
// E — Dispatch from binding_key
// ========================================================================

#[test]
fn e_dispatch_from_binding_key() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Snapshot: system.status → binding_key "builtin.system_status".
    let t_out = OperationSpec {
        name: "stdout.send_text".into(),
        risk: Risk::Write,
        description: "send reply".into(),
        parameters: json!({"type": "object"}),
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.stdout_send_text".into(),
    };
    let t_bound = OperationSpec {
        name: "system.status".into(),
        risk: Risk::ReadOnly,
        description: "system status handler".into(),
        parameters: json!({"type": "object", "additionalProperties": false}),
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.system_status".into(),
    };
    let snap = journal.create_registry_snapshot(vec![t_out, t_bound])?;
    journal.activate_registry_snapshot(&snap.snapshot_id)?;

    // Run: system.status → real status handler.
    let mut cfg = test_config();
    cfg.extra_allowed_operations = vec!["system.status".to_string()];
    let gw = Gateway::new(cfg.clone());
    let llm = ToolCallLlm::new("system.status");
    let rt = Runtime::new(cfg, llm);
    let ev = gw.validate_ingress(&journal, gw.cli_ingress("status".into())?)?;
    let _ = rt.deliver(&journal, &gw, ev)?;

    let events = journal.events()?;
    let succeeded: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
        })
        .collect();
    assert!(
        !succeeded.is_empty(),
        "system.status should have Succeeded Receipts"
    );
    let status_receipt = succeeded.iter().find(|e| {
        e.payload.pointer("/output/status").is_some()
            && e.payload.pointer("/output/event_count").is_some()
    });
    assert!(
        status_receipt.is_some(),
        "A receipt must have 'status' and 'event_count' (status handler)"
    );

    Ok(())
}
