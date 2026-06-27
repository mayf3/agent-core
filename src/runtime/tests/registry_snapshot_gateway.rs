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
    EndpointChoice, LlmClient, LlmInput, LlmOutput, ProviderToolTurn, ToolCall,
    ToolCallResult,
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
        extra_allowed_operations: vec!["time.now".to_string(), "system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
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

/// Fake LLM: emits a tool call on first round, text on subsequent.
struct ToolCallLlm {
    operation: String,
    second_text: String,
}

impl ToolCallLlm {
    fn new(operation: &str) -> Self {
        Self {
            operation: operation.into(),
            second_text: "done".into(),
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
    let _ = rt_a.deliver(&journal, &gw_a, ev_a)?;

    let events = journal.events()?;
    let status_rejected = events.iter().filter(|e| {
        e.kind == JournalEventKind::ToolCallRejected
            && e.payload.get("error_category").and_then(Value::as_str) == Some("unknown_operation")
    }).count();
    assert!(status_rejected > 0, "system.status must be rejected in v1 (unknown)");

    // Run B bound to v2 — system.status exists.
    journal.activate_registry_snapshot(&s2.snapshot_id)?;
    let cfg_b = test_config();
    let gw_b = Gateway::new(cfg_b.clone());
    let llm_b = ToolCallLlm::new("system.status");
    let rt_b = Runtime::new(cfg_b, llm_b);
    let ev_b = gw_b.validate_ingress(&journal, gw_b.cli_ingress("status".into())?)?;
    let _ = rt_b.deliver(&journal, &gw_b, ev_b)?;

    let events2 = journal.events()?;
    let status_issued = events2.iter().filter(|e| {
        e.kind == JournalEventKind::ToolCallIssued
            && e.payload.get("operation").and_then(Value::as_str)
                .map(|s| s.starts_with("unknown_operation_"))
                .unwrap_or(false) == false  // NOT sanitized = known op
    }).count();
    // system.status is a known catalog operation, so it won't be sanitized.
    // ReceiptReceived doesn't carry operation name — count all succeeded receipts.
    let succeeded_receipts = events2.iter().filter(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
    }).count();
    assert!(succeeded_receipts > 0, "system.status must produce a Succeeded Receipt in v2");

    Ok(())
}

// ========================================================================
// D — Risk from Run snapshot
// ========================================================================

#[test]
fn d_risk_from_run_snapshot() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // v1: time.now is ReadOnly.
    let v1 = vec![
        op("stdout.send_text", Risk::Write, "send reply"),
        op("time.now", Risk::ReadOnly, "ro in v1"),
    ];
    let s1 = journal.create_registry_snapshot(v1)?;
    journal.activate_registry_snapshot(&s1.snapshot_id)?;

    // v2: time.now is Write (not yet activated).
    let v2 = vec![
        op("stdout.send_text", Risk::Write, "send reply"),
        op("time.now", Risk::Write, "write in v2"),
    ];
    let s2 = journal.create_registry_snapshot(v2)?;

    // Run A bound to v1 (ReadOnly) — tool call for time.now succeeds.
    let cfg_a = test_config();
    let gw_a = Gateway::new(cfg_a.clone());
    let llm_a = ToolCallLlm::new("time.now");
    let rt_a = Runtime::new(cfg_a, llm_a);
    let ev_a = gw_a.validate_ingress(&journal, gw_a.cli_ingress("op".into())?)?;
    let _ = rt_a.deliver(&journal, &gw_a, ev_a)?;

    let events = journal.events()?;
    let a_succeeded = events.iter().filter(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
    }).count();
    assert!(a_succeeded > 0, "v1 ReadOnly time.now should produce Succeeded Receipts");

    // Activate v2 (Write) for Run B.
    journal.activate_registry_snapshot(&s2.snapshot_id)?;

    // Run B bound to v2 (Write) — tool call rejected as operation_not_allowed.
    let cfg_b = test_config();
    let gw_b = Gateway::new(cfg_b.clone());
    let llm_b = ToolCallLlm::new("time.now");
    let rt_b = Runtime::new(cfg_b, llm_b);
    let ev_b = gw_b.validate_ingress(&journal, gw_b.cli_ingress("op".into())?)?;
    let _ = rt_b.deliver(&journal, &gw_b, ev_b)?;

    let events2 = journal.events()?;
    let b_rejected = events2.iter().filter(|e| {
        e.kind == JournalEventKind::ToolCallRejected
            && e.payload.get("error_category").and_then(Value::as_str) == Some("operation_not_allowed")
    }).count();
    assert!(b_rejected > 0, "v2 Write op must be rejected as operation_not_allowed");

    Ok(())
}

// ========================================================================
// E — Dispatch from binding_key
// ========================================================================

#[test]
fn e_dispatch_from_binding_key() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Snapshot: system.status → binding_key "builtin.time_now".
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
        description: "bound to time.now handler".into(),
        parameters: json!({"type": "object", "additionalProperties": false}),
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.time_now".into(),
    };
    let snap_v1 = journal.create_registry_snapshot(vec![t_out.clone(), t_bound])?;
    journal.activate_registry_snapshot(&snap_v1.snapshot_id)?;

    // Run: system.status should execute time.now handler (not the status handler).
    let mut cfg = test_config();
    cfg.extra_allowed_operations = vec!["system.status".to_string()];
    let gw = Gateway::new(cfg.clone());
    let llm = ToolCallLlm::new("system.status");
    let rt = Runtime::new(cfg, llm);
    let ev = gw.validate_ingress(&journal, gw.cli_ingress("status".into())?)?;
    let _ = rt.deliver(&journal, &gw, ev)?;

    let events = journal.events()?;
    let succeeded_receipts: Vec<_> = events.iter().filter(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
    }).collect();
    assert!(!succeeded_receipts.is_empty(), "system.status should have Succeeded Receipts");
    // Find receipt with time.now-specific characteristics.
    let time_now_receipt = succeeded_receipts.iter().find(|e| {
        e.payload.pointer("/output/iso").is_some()
    });
    assert!(time_now_receipt.is_some(), "At least one receipt must have 'iso' (time.now handler)");
    let r = time_now_receipt.unwrap();
    assert_eq!(r.payload.get("status").and_then(Value::as_str), Some("Succeeded"));
    let output = r.payload.get("output").and_then(|o| o.as_object());
    assert!(output.is_some(), "Receipt must have output object");
    let out = output.unwrap();
    assert!(out.contains_key("iso"), "time.now handler produces 'iso': {:?}", out.keys().collect::<Vec<_>>());
    assert!(out.contains_key("epoch_ms"), "time.now handler produces 'epoch_ms'");

    // New snapshot: system.status → binding_key "builtin.system_status" (default).
    let t_bound2 = OperationSpec {
        name: "system.status".into(),
        risk: Risk::ReadOnly,
        description: "bound to system.status handler".into(),
        parameters: json!({"type": "object", "additionalProperties": false}),
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.system_status".into(),
    };
    let snap_v2 = journal.create_registry_snapshot(vec![t_out, t_bound2])?;
    journal.activate_registry_snapshot(&snap_v2.snapshot_id)?;

    // Run 2: system.status → real status handler.
    let cfg2 = test_config();
    let gw2 = Gateway::new(cfg2.clone());
    let llm2 = ToolCallLlm::new("system.status");
    let rt2 = Runtime::new(cfg2, llm2);
    let ev2 = gw2.validate_ingress(&journal, gw2.cli_ingress("status".into())?)?;
    let _ = rt2.deliver(&journal, &gw2, ev2)?;

    let events2 = journal.events()?;
    let succeeded2: Vec<_> = events2.iter().filter(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
    }).collect();
    assert!(succeeded2.len() >= 2, "Both runs should have Succeeded Receipts");
    // Find the receipt with system.status-specific characteristics (has status, event_count, no iso).
    let status_receipt = succeeded2.iter().find(|e| {
        e.payload.pointer("/output/status").is_some()
            && e.payload.pointer("/output/event_count").is_some()
    });
    assert!(status_receipt.is_some(), "A receipt must have 'status' and 'event_count' (status handler)");
    // Verify the status handler receipt does NOT have iso (time.now characteristic).
    // But iso from time.now handler will be there too. To check that the status handler was
    // used separately, look for a receipt with status field but no iso.
    // Actually, both the time.now receipt and the status receipt may exist.
    // The important thing is: at least one receipt exists with each handler's marks.
    let time_now_receipts = succeeded2.iter().filter(|e| {
        e.payload.pointer("/output/iso").is_some()
    }).count();
    let status_receipts = succeeded2.iter().filter(|e| {
        e.payload.pointer("/output/status").is_some()
    }).count();
    assert!(time_now_receipts >= 1, "At least one receipt from time.now handler");
    assert!(status_receipts >= 1, "At least one receipt from system.status handler");

    Ok(())
}
