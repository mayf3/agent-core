//! Auto-recovery and schema fidelity tests for coding tool validation.
//! These tests verify the complete recovery chain:
//! missing workspace_id → structured rejection → follow-up LLM retries → success

use super::super::Runtime;
use super::tool_loop_tests::test_config;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use serde_json::{json, Value};
use std::sync::atomic::AtomicUsize;

// ── Helper: register an operation ──

fn register_external_op(
    j: &JournalStore,
    g: &Gateway,
    op: &str,
    input_schema: Value,
    output_schema: Value,
) -> String {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "t".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:1/execute".into(),
        operation_name: op.into(),
        description: op.into(),
        input_schema,
        output_schema,
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    let mid = m.compute_manifest_id().unwrap();
    m.manifest_id = mid.clone();
    j.register_harness_manifest(&m).unwrap();
    j.enable_harness(
        &g.approve_harness_change(HarnessChangeIntent {
            action: HarnessChangeAction::Enable,
            manifest_id: mid.clone(),
            expected_snapshot_id: j.current_registry_snapshot_id().unwrap(),
            requested_by: "ipc_operator".into(),
        })
        .unwrap(),
    )
    .unwrap();
    mid
}
/// LLM that auto-recovers from missing workspace_id.
/// Round 0: calls propose WITHOUT workspace_id.
/// Round 1: reads ToolResult, sees missing_fields, retries with workspace_id.
struct AutoRecoveryLlm {
    round: AtomicUsize,
}

impl LlmClient for AutoRecoveryLlm {
    fn complete(&self, input: LlmInput) -> anyhow::Result<LlmOutput> {
        let r = self
            .round
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match r {
            0 => Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "call propose".into(),
                journal_payload: json!({"r":0}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "call_0".into(),
                    operation: "external.coding_capability_propose".into(),
                    arguments: json!({"artifact_path":"artifact.bin","manifest_path":"manifest.json","evidence_path":"evidence.json"}),
                }),
                provider_turn: None,
            }),
            1 => {
                let should_retry = input.blocks.iter().any(|b| {
                    matches!(b.kind, ContextBlockKind::ToolResult)
                        && b.content.contains("missing_fields")
                        && b.content.contains("workspace_id")
                });
                if should_retry {
                    Ok(LlmOutput {
                        provider: "t".into(),
                        model: "t".into(),
                        content: "retry with ws".into(),
                        journal_payload: json!({"r":1}),
                        tool_call: ToolCallResult::Valid(ToolCall {
                            id: "call_1".into(),
                            operation: "external.coding_capability_propose".into(),
                            arguments: json!({"workspace_id":"agent-dev","artifact_path":"artifact.bin","manifest_path":"manifest.json","evidence_path":"evidence.json"}),
                        }),
                        provider_turn: None,
                    })
                } else {
                    Ok(LlmOutput {
                        provider: "t".into(),
                        model: "t".into(),
                        content: "no retry".into(),
                        journal_payload: json!({"r":1}),
                        tool_call: ToolCallResult::Absent,
                        provider_turn: None,
                    })
                }
            }
            _ => Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "done".into(),
                journal_payload: json!({"r":r}),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            }),
        }
    }
}
#[test]
fn missing_workspace_id_is_repaired_within_same_run() {
    let mut config = test_config();
    config.max_tool_rounds = 5;
    let j = JournalStore::in_memory().unwrap();
    let g = Gateway::new(config.clone());
    let runtime = Runtime::new(
        config,
        AutoRecoveryLlm {
            round: AtomicUsize::new(0),
        },
    );

    let cp_schema = json!({
        "type": "object",
        "properties": {
            "workspace_id": {"type":"string","description":"ws","enum":["agent-dev"]},
            "artifact_path": {"type":"string"},"manifest_path": {"type":"string"},"evidence_path": {"type":"string"}
        },
        "required": ["workspace_id","artifact_path","manifest_path","evidence_path"],
        "additionalProperties": false
    });
    register_external_op(
        &j,
        &g,
        "external.coding_capability_propose",
        cp_schema,
        json!({"type":"object"}),
    );

    let event = g
        .validate_ingress(&j, g.cli_ingress("propose it".into()).unwrap())
        .unwrap();
    let outcome = runtime.deliver(&j, &g, event).unwrap();
    let events = j.events().unwrap();

    // 2 tool calls: 1st fails validation, 2nd retries
    let issued: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::ToolCallIssued)
        .collect();
    assert_eq!(issued.len(), 2, "two tool calls issued");
    let rejected: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::ToolCallRejected)
        .collect();
    assert_eq!(rejected.len(), 1, "first call rejected by schema");
    // Run not Failed
    assert_ne!(
        j.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Failed")
    );
    assert!(events.iter().all(|e| e.kind != JournalEventKind::RunFailed));
    assert!(j.verify_hash_chain().unwrap());
}
/// LLM that fails follow-up after missing workspace rejection.
struct RecoveryThenFollowupFailsLlm {
    round: AtomicUsize,
}

impl LlmClient for RecoveryThenFollowupFailsLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        let r = self
            .round
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match r {
            0 => Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "propose".into(),
                journal_payload: json!({"r":0}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "c0".into(),
                    operation: "external.coding_capability_propose".into(),
                    arguments: json!({"artifact_path":"a.bin","manifest_path":"m.json","evidence_path":"e.json"}),
                }),
                provider_turn: None,
            }),
            _ => anyhow::bail!("follow-up LLM simulated failure"),
        }
    }
}
#[test]
fn missing_workspace_rejection_then_followup_llm_failure_notifies_user() {
    let mut config = test_config();
    config.max_tool_rounds = 5;
    let j = JournalStore::in_memory().unwrap();
    let g = Gateway::new(config.clone());
    let runtime = Runtime::new(
        config,
        RecoveryThenFollowupFailsLlm {
            round: AtomicUsize::new(0),
        },
    );

    let cp_schema = json!({
        "type":"object","properties":{
            "workspace_id":{"type":"string","description":"ws","enum":["agent-dev"]},
            "artifact_path":{"type":"string"},"manifest_path":{"type":"string"},"evidence_path":{"type":"string"}
        },"required":["workspace_id","artifact_path","manifest_path","evidence_path"],
        "additionalProperties":false
    });
    register_external_op(
        &j,
        &g,
        "external.coding_capability_propose",
        cp_schema,
        json!({"type":"object"}),
    );

    let event = g
        .validate_ingress(&j, g.cli_ingress("propose".into()).unwrap())
        .unwrap();
    let outcome = runtime.deliver(&j, &g, event).unwrap();
    let events = j.events().unwrap();

    assert_eq!(
        j.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Failed")
    );
    let failed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::RunFailed)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].payload["error_category"],
        "tool_followup_llm_failed"
    );
    assert!(events
        .iter()
        .all(|e| e.kind != JournalEventKind::RunCompleted));
    let oq: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .collect();
    assert_eq!(oq.len(), 1, "failure reply enqueued");
    let rej: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::ToolCallRejected)
        .collect();
    assert_eq!(rej.len(), 1);
    assert!(outcome.output.contains("模型生成后续回复时失败了"));
    assert!(!outcome.output.contains("provider"));
    assert!(j.verify_hash_chain().unwrap());
}
/// Schema fidelity test: canonical spec → Snapshot → provider_tools → LLM definition.
#[test]
fn coding_manifest_schema_reaches_llm_tool_definition_intact() {
    let cp_schema = json!({
        "type":"object","properties":{
            "workspace_id":{"type":"string","description":"ws","enum":["agent-dev"]},
            "artifact_path":{"type":"string"},"manifest_path":{"type":"string"},"evidence_path":{"type":"string"}
        },"required":["workspace_id","artifact_path","manifest_path","evidence_path"],
        "additionalProperties":false
    });
    let write_schema = json!({
        "type":"object","properties":{
            "workspace_id":{"type":"string","description":"ws","enum":["agent-dev"]},
            "relative_path":{"type":"string"},"content":{"type":"string"},
            "mode":{"type":"string","enum":["replace","append"]}
        },"required":["workspace_id","relative_path","content"],
        "additionalProperties":false
    });
    let submit_schema = json!({
        "type":"object","properties":{
            "workspace_id":{"type":"string","description":"ws","enum":["agent-dev"]},
            "backend":{"type":"string","enum":["opencode"]},"objective":{"type":"string"}
        },"required":["workspace_id","backend","objective"],
        "additionalProperties":false
    });
    let task_status_schema = json!({
        "type":"object","properties":{"task_id":{"type":"string"}},
        "required":["task_id"],"additionalProperties":false
    });

    // Test capability.propose via provider_tools_for_grants.
    let snap = crate::registry::snapshot::RegistrySnapshot {
        snapshot_id: "snap_fidelity".to_string(),
        created_at: chrono::Utc::now(),
        operations: vec![crate::registry::snapshot::OperationSpec {
            name: "external.coding_capability_propose".into(),
            risk: crate::registry::snapshot::Risk::ReadOnly,
            description: "propose".into(),
            parameters: cp_schema.clone(),
            idempotent: true,
            binding_kind: crate::registry::snapshot::BindingKind::External,
            binding_key: "man_propose".into(),
        }],
    };
    let provider_tools =
        snap.provider_tools_for_grants(&["external.coding_capability_propose".to_string()]);
    assert!(!provider_tools.is_empty());

    let cp_tool = provider_tools
        .iter()
        .find(|t| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                == Some("external.coding_capability_propose")
        })
        .expect("capability.propose in provider tools");
    let func = cp_tool.get("function").expect("function");
    let params = func.get("parameters").expect("parameters");

    assert!(!func
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty());
    assert_eq!(params.get("type").and_then(Value::as_str), Some("object"));
    let required: Vec<&str> = params
        .get("required")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|v: &Value| v.as_str())
        .collect();
    assert!(required.contains(&"workspace_id"));
    assert!(required.contains(&"artifact_path"));
    assert!(required.contains(&"manifest_path"));
    assert!(required.contains(&"evidence_path"));
    assert_eq!(required.len(), 4);
    assert_eq!(
        params.get("additionalProperties").and_then(Value::as_bool),
        Some(false)
    );
    let ws_id = params.pointer("/properties/workspace_id").unwrap();
    assert_eq!(ws_id.get("type").and_then(Value::as_str), Some("string"));
    let ws_enum = ws_id.get("enum").and_then(Value::as_array).unwrap();
    assert!(ws_enum.contains(&json!("agent-dev")));

    // Check additional schemas
    let mode = write_schema.pointer("/properties/mode").unwrap();
    let mode_enum = mode.get("enum").and_then(Value::as_array).unwrap();
    assert!(mode_enum.contains(&json!("replace")));
    assert!(mode_enum.contains(&json!("append")));

    let backend = submit_schema.pointer("/properties/backend").unwrap();
    let be_enum = backend.get("enum").and_then(Value::as_array).unwrap();
    assert!(be_enum.contains(&json!("opencode")));

    let tstat_required: Vec<&str> = task_status_schema
        .get("required")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|v: &Value| v.as_str())
        .collect();
    assert!(tstat_required.contains(&"task_id"));
    assert!(!tstat_required.contains(&"workspace_id"));
}