use super::super::recall_test_support::{count_kind, feishu_envelope, process_outbox, test_config};
use super::super::Runtime;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
fn register_external_op_with_endpoint(
    j: &JournalStore,
    g: &Gateway,
    op: &str,
    input_schema: Value,
    output_schema: Value,
    endpoint: &str,
) -> String {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "t".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: endpoint.to_string(),
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
struct AutoRecoveryLlm {
    round: AtomicUsize,
}
impl LlmClient for AutoRecoveryLlm {
    fn complete(&self, input: LlmInput) -> anyhow::Result<LlmOutput> {
        let r = self.round.fetch_add(1, Ordering::Relaxed);
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
                    if !matches!(b.kind, ContextBlockKind::ToolResult) {
                        return false;
                    }
                    // Content format: "tool: op\nresult: {json}". Extract JSON.
                    let json_str = b
                        .content
                        .split_once("\nresult: ")
                        .map(|(_, j)| j)
                        .unwrap_or(&b.content);
                    serde_json::from_str::<Value>(json_str)
                        .ok()
                        .map_or(false, |v| {
                            let missing = v
                                .get("details")
                                .and_then(|d| d.get("missing_fields"))
                                .and_then(|a| a.as_array())
                                .map(|a| a.iter().any(|f| f == "workspace_id"))
                                .unwrap_or(false);
                            let available = v
                                .get("details")
                                .and_then(|d| d.get("available_workspace_ids"))
                                .and_then(|a| a.as_array())
                                .map(|a| a.iter().any(|id| id == "agent-dev"))
                                .unwrap_or(false);
                            missing && available
                        })
                });
                if should_retry {
                    Ok(LlmOutput {
                        provider: "t".into(),
                        model: "t".into(),
                        content: "retry".into(),
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
    let harness_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let harness_port = harness_listener.local_addr().unwrap().port();
    let harness_endpoint = format!("http://127.0.0.1:{harness_port}/execute");
    let call_count = Arc::new(AtomicUsize::new(0));
    let cc = call_count.clone();
    thread::spawn(move || {
        for stream in harness_listener.incoming() {
            if let Ok(mut s) = stream {
                cc.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let body = r#"{"protocol_version":"external-harness-v1","ok":true,"result":{"proposal_id":"test","status":"PendingApproval"}}"#;
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
            }
        }
    });
    thread::sleep(Duration::from_millis(50));
    let mut config = test_config();
    config.feishu_coding_owner_id = Some("owner".into());
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
        "type":"object","properties":{
            "workspace_id":{"type":"string","description":"ws","enum":["agent-dev"]},
            "artifact_path":{"type":"string"},"manifest_path":{"type":"string"},"evidence_path":{"type":"string"}
        },"required":["workspace_id","artifact_path","manifest_path","evidence_path"],
        "additionalProperties":false
    });
    register_external_op_with_endpoint(
        &j,
        &g,
        "external.coding_capability_propose",
        cp_schema,
        json!({"type":"object"}),
        &harness_endpoint,
    );
    let event = g
        .validate_ingress(
            &j,
            serde_json::from_value(feishu_envelope("e2", "m2", "owner", "c2", "propose it"))
                .unwrap(),
        )
        .unwrap();
    let outcome = runtime.deliver(&j, &g, event).unwrap();
    process_outbox(&j, &outcome.run_id);
    let events = j.events().unwrap();
    assert_eq!(
        count_kind(&events, JournalEventKind::ToolCallIssued),
        2,
        "two tool calls"
    );
    assert_eq!(
        count_kind(&events, JournalEventKind::ToolCallRejected),
        1,
        "first call rejected"
    );
    assert_eq!(call_count.load(Ordering::SeqCst), 1, "harness called once");
    assert!(
        events
            .iter()
            .any(|e| e.kind == JournalEventKind::ReceiptReceived
                && e.payload["status"] == "Succeeded")
    );
    assert_eq!(
        count_kind(&events, JournalEventKind::RunCompleted),
        1,
        "RunCompleted"
    );
    assert_eq!(
        count_kind(&events, JournalEventKind::RunFailed),
        0,
        "no RunFailed"
    );
    assert_eq!(
        j.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Completed")
    );
    assert!(j.verify_hash_chain().unwrap());
}
struct RecoveryThenFollowupFailsLlm {
    round: AtomicUsize,
}
impl LlmClient for RecoveryThenFollowupFailsLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        let r = self.round.fetch_add(1, Ordering::Relaxed);
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
    register_external_op_with_endpoint(
        &j,
        &g,
        "external.coding_capability_propose",
        cp_schema,
        json!({"type":"object"}),
        "http://127.0.0.1:1/execute",
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
    assert_eq!(count_kind(&events, JournalEventKind::RunFailed), 1);
    assert_eq!(
        events
            .iter()
            .find(|e| e.kind == JournalEventKind::RunFailed)
            .unwrap()
            .payload["error_category"],
        "tool_followup_llm_failed"
    );
    assert_eq!(count_kind(&events, JournalEventKind::RunCompleted), 0);
    let oq = count_kind(&events, JournalEventKind::OutboxQueued);
    assert_eq!(oq, 1, "failure reply enqueued");
    assert_eq!(count_kind(&events, JournalEventKind::ToolCallRejected), 1);
    assert!(outcome.output.contains("模型生成后续回复时失败了"));
    assert!(!outcome.output.contains("provider"));
    assert!(j.verify_hash_chain().unwrap());
}
/// Schema fidelity: register → activate → deliver → LlmInput.
struct SchemaFidelityLlm {
    captured_tools: Arc<std::sync::Mutex<Vec<Value>>>,
}
impl LlmClient for SchemaFidelityLlm {
    fn complete(&self, input: LlmInput) -> anyhow::Result<LlmOutput> {
        let mut tools = self.captured_tools.lock().unwrap();
        if tools.is_empty() {
            *tools = input.provider_tools.clone();
        }
        drop(tools);
        Ok(LlmOutput {
            provider: "t".into(),
            model: "t".into(),
            content: "ok".into(),
            journal_payload: json!({"r":0}),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}
#[test]
fn coding_manifest_schema_reaches_llm_tool_definition_intact() {
    let mut cfg = test_config();
    cfg.feishu_coding_owner_id = Some("owner".into());
    let j = JournalStore::in_memory().unwrap();
    let g = Gateway::new(cfg.clone());
    let store = Arc::new(std::sync::Mutex::new(Vec::new()));
    let runtime = Runtime::new(
        cfg,
        SchemaFidelityLlm {
            captured_tools: store.clone(),
        },
    );
    let ws_prop = json!({"type":"string","description":"授权 workspace 的 ID。当前可用 workspace: agent-dev","enum":["agent-dev"]});
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "t".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:1/execute".into(),
        operation_name: "external.coding_capability_propose".into(),
        description: "Capability Proposal".into(),
        input_schema: json!({"type":"object","properties":{"workspace_id":ws_prop,"artifact_path":{"type":"string","description":"artifact path"},"manifest_path":{"type":"string","description":"manifest path"},"evidence_path":{"type":"string","description":"evidence path"}},"required":["workspace_id","artifact_path","manifest_path","evidence_path"],"additionalProperties":false}),
        output_schema: json!({"type":"object"}),
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
    let event = g
        .validate_ingress(
            &j,
            serde_json::from_value(feishu_envelope("e1", "m1", "owner", "c1", "test")).unwrap(),
        )
        .unwrap();
    let outcome = runtime.deliver(&j, &g, event).unwrap();
    process_outbox(&j, &outcome.run_id);
    let captured = store.lock().unwrap();
    assert!(!captured.is_empty(), "provider_tools captured");
    let cp_tool = captured
        .iter()
        .find(|t| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                == Some("external.coding_capability_propose")
        })
        .expect("capability.propose in provider tools");
    let func = cp_tool.get("function").expect("function");
    assert!(!func
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty());
    let params = func.get("parameters").expect("parameters");
    assert_eq!(params.get("type").and_then(Value::as_str), Some("object"));
    let required: Vec<&str> = params
        .get("required")
        .unwrap()
        .as_array()
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
    assert!(j.verify_hash_chain().unwrap());
}
