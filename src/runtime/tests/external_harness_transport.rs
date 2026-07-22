//! External harness strict HTTP status line parsing tests.
//! These use a simple TcpListener fixture and invoke the Runtime
//! with a tool-calling LLM to verify transport-level behavior.

use anyhow::Result;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::domain::{CapabilityGrant, ValidatedEvent};

fn event_with_time_grant(
    j: &crate::journal::JournalStore,
    g: &crate::gateway::Gateway,
) -> ValidatedEvent {
    let mut ev = g
        .validate_ingress(j, g.cli_ingress("t?".into()).unwrap())
        .unwrap();
    ev.principal.grants.push(CapabilityGrant {
        operation: "external.time_now".to_string(),
        scope: "current_session".to_string(),
    });
    ev
}

fn config() -> crate::config::KernelConfig {
    crate::config::KernelConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        data_dir: std::path::PathBuf::from(".agent-core-test"),
        agent_id: crate::domain::AgentId("main".to_string()),
        root_dir: std::path::PathBuf::from("."),
        kernel_port: 0,
        connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        coding_harness_api_url: "http://127.0.0.1:7200".into(),
        coding_harness_artifact_digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
    }
}

struct OneToolLlm {
    first: AtomicBool,
}
impl crate::llm::LlmClient for OneToolLlm {
    fn complete(&self, _: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        if self.first.swap(false, Ordering::SeqCst) {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                    id: "c".into(),
                    operation: "external.time_now".into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(crate::llm::ProviderToolTurn {
                    endpoint: crate::llm::EndpointChoice::Primary,
                    provider_tool_call_id: "cr".into(),
                    wire_name: "external.time_now".into(),
                    canonical_operation: "external.time_now".into(),
                    reasoning_content: None,
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "done".into(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

// CaptureToolsLlm for non-2xx round-2 assertions

struct CaptureLlm {
    captured: Arc<Mutex<Vec<serde_json::Value>>>,
    first: AtomicBool,
}
impl crate::llm::LlmClient for CaptureLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.captured.lock().unwrap().push(json!({
            "provider_tools": input.provider_tools,
            "follow_up_count": input.follow_ups.len(),
            "follow_ups": captured_follow_ups(&input),
        }));
        if self.first.swap(false, Ordering::SeqCst) {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                    id: "c".into(),
                    operation: "external.time_now".into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(crate::llm::ProviderToolTurn {
                    endpoint: crate::llm::EndpointChoice::Primary,
                    provider_tool_call_id: "cr".into(),
                    wire_name: "external.time_now".into(),
                    canonical_operation: "external.time_now".into(),
                    reasoning_content: None,
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "done".into(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

/// Run a harness tool call through Runtime::deliver with captured
/// LlmInput rounds so the caller can inspect round-2 ToolResult.
fn run_with_capture(
    j: &crate::journal::JournalStore,
    g: &crate::gateway::Gateway,
) -> (super::RuntimeOutcome, Arc<Mutex<Vec<serde_json::Value>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let c2 = captured.clone();
    let rt = super::Runtime::new(
        config(),
        CaptureLlm {
            captured: c2,
            first: AtomicBool::new(true),
        },
    );
    let ev = event_with_time_grant(j, g);
    (rt.deliver(j, g, ev).unwrap(), captured)
}

fn captured_follow_ups(input: &crate::llm::LlmInput) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = input
        .follow_ups
        .iter()
        .map(|fu| {
            let turn = &fu.provider_turn;
            json!({
                "provider_turn": {
                    "endpoint": format!("{:?}", turn.endpoint),
                    "provider_tool_call_id": turn.provider_tool_call_id,
                    "wire_name": turn.wire_name,
                    "canonical_operation": turn.canonical_operation,
                },
                "result_content": fu.result_content,
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}

fn start_responder(response: &str) -> Result<(String, Arc<AtomicBool>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let endpoint = format!("http://127.0.0.1:{port}/execute");
    let shutdown = Arc::new(AtomicBool::new(false));
    let resp = response.to_string();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    thread::sleep(Duration::from_millis(50));
    Ok((endpoint, shutdown))
}

fn h200(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
}

fn reg_enable(
    j: &crate::journal::JournalStore,
    g: &crate::gateway::Gateway,
    ep: &str,
) -> Result<String> {
    use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
    use crate::harness::manifest::HarnessManifest;
    use chrono::Utc;
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "t".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ep.into(),
        operation_name: "external.time_now".into(),
        description: "time".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"iso":{"type":"string"},"epoch_ms":{"type":"integer"}},"required":["iso","epoch_ms"],"additionalProperties":false}),
        idempotent: true,
        created_at: Utc::now(),
    };
    let mid = m.compute_manifest_id()?;
    m.manifest_id = mid.clone();
    j.register_harness_manifest(&m)?;
    j.enable_harness(&g.approve_harness_change(HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: mid.clone(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    })?)?;
    Ok(mid)
}

fn run_with_tool(
    j: &crate::journal::JournalStore,
    g: &crate::gateway::Gateway,
) -> super::RuntimeOutcome {
    let rt = super::Runtime::new(
        config(),
        OneToolLlm {
            first: AtomicBool::new(true),
        },
    );
    let ev = event_with_time_grant(j, g);
    rt.deliver(j, g, ev).unwrap()
}

// ── Tests ──

#[test]
fn http_200_is_success() -> Result<()> {
    let b = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"2026-01-01T00:00:00+00:00","epoch_ms":123}});
    let (ep, _) = start_responder(&h200(&b.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    reg_enable(&j, &g, &ep)?;
    let o = run_with_tool(&j, &g);
    assert!(!o.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    Ok(())
}

#[test]
fn http_302_is_http_error() -> Result<()> {
    let (ep,_) = start_responder("HTTP/1.1 302 Found\r\nLocation: http://evil/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    reg_enable(&j, &g, &ep)?;
    run_with_tool(&j, &g);
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(r[0].payload["output"]["error_category"], "http_error");
    assert_eq!(r[0].payload["output"]["http_code"], 302);
    Ok(())
}

#[test]
fn http_404_is_http_error() -> Result<()> {
    let (ep, _) = start_responder(
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    reg_enable(&j, &g, &ep)?;
    run_with_tool(&j, &g);
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(r[0].payload["output"]["error_category"], "http_error");
    assert_eq!(r[0].payload["output"]["http_code"], 404);
    Ok(())
}

#[test]
fn http_500_is_http_error() -> Result<()> {
    let (ep, _) = start_responder(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    reg_enable(&j, &g, &ep)?;
    let (_o, captured) = run_with_capture(&j, &g);
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(r[0].payload["output"]["error_category"], "http_error");
    assert_eq!(r[0].payload["output"]["http_code"], 500);
    // Round-2 failed ToolResult assertion
    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2, "LLM called twice");
    assert_eq!(caps[1]["follow_up_count"].as_u64().unwrap_or(0), 1);
    let fu = &caps[1]["follow_ups"][0];
    let rc = fu["result_content"].as_str().unwrap_or("");
    assert!(
        rc.contains("execution_failed"),
        "non-2xx ToolResult must contain execution_failed"
    );
    Ok(())
}

#[test]
fn http_malformed_status_line_is_malformed() -> Result<()> {
    let (ep, _) = start_responder("NOT_HTTP\r\nContent-Length: 0\r\n\r\n")?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    reg_enable(&j, &g, &ep)?;
    run_with_tool(&j, &g);
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(
        r[0].payload["output"]["error_category"],
        "malformed_response"
    );
    Ok(())
}

#[test]
fn harness_error_code_is_mapped_to_fixed_category() -> Result<()> {
    let b = json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"custom_db_error_123"});
    let (ep, _) = start_responder(&h200(&b.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    reg_enable(&j, &g, &ep)?;
    run_with_tool(&j, &g);
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(
        r[0].payload["output"]["error_category"],
        "external_infrastructure_failure"
    );
    assert_eq!(r[0].payload["output"]["detail_code"], "custom_db_error_123");
    Ok(())
}
