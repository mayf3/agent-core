//! Runtime e2e tests for external harness: real Runtime::deliver flows,
//! secret/unknown field scanning, request integrity, and failure paths.

use anyhow::Result;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
    }
}

// ── Helpers ──

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

fn harness_200(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
}

fn register_and_enable(
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
    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: mid.clone(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    };
    j.enable_harness(&g.approve_harness_change(intent)?)?;
    Ok(mid)
}

struct CaptureToolsLlm {
    captured: Arc<Mutex<Vec<serde_json::Value>>>,
    first: AtomicBool,
}

impl crate::llm::LlmClient for CaptureToolsLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.captured.lock().unwrap().push(json!({"provider_tools": input.provider_tools, "follow_up_count": input.follow_ups.len()}));
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
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "done".into(),
                journal_payload: json!({"s":"ok","c":"done"}),
                tool_call: crate::llm::ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

// ── Runtime e2e test ──

#[test]
fn external_harness_tool_call_runs_end_to_end() -> Result<()> {
    let body = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"2026-06-30T12:00:00+00:00","epoch_ms":1234567890}});
    let (ep, _) = start_responder(&harness_200(&body.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    let s1 = j.current_registry_snapshot_id()?;
    register_and_enable(&j, &g, &ep)?;
    let s2 = j.current_registry_snapshot_id()?;
    assert_ne!(s1, s2);

    let captured = Arc::new(Mutex::new(Vec::new()));
    let llm = CaptureToolsLlm {
        captured: captured.clone(),
        first: AtomicBool::new(true),
    };
    let rt = super::Runtime::new(config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("t?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());

    let run = j.run(&outcome.run_id)?.expect("run exists");
    assert_eq!(run.registry_snapshot_id, s2, "Run must pin to S2");
    assert!(
        run.principal
            .grants
            .iter()
            .any(|g| g.operation == "external.time_now"),
        "Run must have grant"
    );

    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2, "LLM called twice");
    let has_tool = caps[0]["provider_tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .any(|t| t["function"]["name"] == "external.time_now")
        })
        .unwrap_or(false);
    assert!(has_tool, "Round 1 tools include external.time_now");
    assert_eq!(
        caps[1]["follow_up_count"].as_u64().unwrap_or(0),
        1,
        "Round 2 has follow-up"
    );

    let ev = j.events()?;
    let ti = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ToolCallIssued)
        .count();
    let ip = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::InvocationProposed)
        .count();
    let ia = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::InvocationApproved)
        .count();
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(ti, 1, "ToolCallIssued == 1");
    assert_eq!(ip, 2, "InvocationProposed == 2 (tool + reply)");
    assert_eq!(ia, 2, "InvocationApproved == 2 (tool + reply)");
    assert_eq!(r.len(), 1, "ReceiptReceived == 1");
    assert_eq!(r[0].payload["status"], "Succeeded");
    assert!(r[0].payload["output"]["iso"].is_string());
    assert!(r[0].payload["output"]["epoch_ms"].is_number());
    Ok(())
}

// ── Secret fields ──

#[test]
fn harness_secret_fields_are_not_in_journal() -> Result<()> {
    let body = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1},
        "SECRET_TOKEN_MARKER":"x","fake_receipt":{"s":"ok"},"fake_journal_event":{"kind":"RC"},"fake_status":"ok","external_ref":"/private/path"});
    let (ep, _) = start_responder(&harness_200(&body.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    let llm = CaptureToolsLlm {
        captured: Arc::new(Mutex::new(Vec::new())),
        first: AtomicBool::new(true),
    };
    let rt = super::Runtime::new(config(), llm);
    rt.deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let s = serde_json::to_string(&j.events()?).unwrap_or_default();
    for f in &[
        "SECRET_TOKEN_MARKER",
        "/private/internal/path",
        "fake_receipt",
        "fake_journal_event",
        "fake_status",
    ] {
        assert!(!s.contains(f), "leaked {f}");
    }
    Ok(())
}

#[test]
fn harness_extra_fields_in_result_cause_schema_violation() -> Result<()> {
    let body = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1,"extra":"bad"}});
    let (ep, _) = start_responder(&harness_200(&body.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    let llm = CaptureToolsLlm {
        captured: Arc::new(Mutex::new(Vec::new())),
        first: AtomicBool::new(true),
    };
    let rt = super::Runtime::new(config(), llm);
    rt.deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(
        r[0].payload["output"]["error_category"],
        "output_schema_violation"
    );
    Ok(())
}

#[test]
fn harness_request_contains_no_internal_fields() -> Result<()> {
    let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let cb = captured.clone();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let ep = format!("http://127.0.0.1:{port}/execute");
    thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut b = [0u8; 4096];
            let n = s.read(&mut b).unwrap_or(0);
            *cb.lock().unwrap() = String::from_utf8_lossy(&b[..n]).to_string();
            let _ = s.write_all(harness_200(r#"{"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1}}"#).as_bytes());
        }
    });
    thread::sleep(Duration::from_millis(50));
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    let llm = CaptureToolsLlm {
        captured: Arc::new(Mutex::new(Vec::new())),
        first: AtomicBool::new(true),
    };
    let rt = super::Runtime::new(config(), llm);
    rt.deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let raw = captured.lock().unwrap();
    let bs = raw.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    let jb = &raw[bs..];
    let p: serde_json::Value = serde_json::from_str(jb).unwrap_or_default();
    assert!(p
        .get("arguments")
        .and_then(|a| a.get("session_id"))
        .is_none());
    assert!(!jb.contains("test-token"));
    Ok(())
}

#[test]
fn harness_ok_false_through_runtime_records_failed() -> Result<()> {
    let b =
        json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"rate_limited"});
    let (ep, _) = start_responder(&harness_200(&b.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    let llm = CaptureToolsLlm {
        captured: Arc::new(Mutex::new(Vec::new())),
        first: AtomicBool::new(true),
    };
    let rt = super::Runtime::new(config(), llm);
    rt.deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(r[0].payload["output"]["error_category"], "harness_failed");
    Ok(())
}

#[test]
fn harness_timeout_through_runtime_records_failed() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let ep = format!("http://127.0.0.1:{port}/execute");
    thread::spawn(move || {
        if let Ok((_, _)) = listener.accept() {
            thread::sleep(Duration::from_millis(500));
        }
    });
    thread::sleep(Duration::from_millis(50));
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    let llm = CaptureToolsLlm {
        captured: Arc::new(Mutex::new(Vec::new())),
        first: AtomicBool::new(true),
    };
    let rt = super::Runtime::new(config(), llm);
    rt.deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    Ok(())
}
