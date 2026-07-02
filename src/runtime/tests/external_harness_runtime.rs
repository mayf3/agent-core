//! Runtime e2e tests for external harness: real Runtime::deliver flows,
//! secret/unknown field scanning, request integrity, and failure paths.

use anyhow::Result;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub(super) fn config() -> crate::config::KernelConfig {
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
        capability_submit_token: String::new(),
        capability_decision_token: String::new(),
    }
}

// ── Helpers ──

pub(super) fn start_responder(response: &str) -> Result<(String, Arc<AtomicBool>)> {
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

pub(super) fn harness_200(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
}

pub(super) fn register_and_enable(
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

pub(super) struct CaptureToolsLlm {
    pub(super) captured: Arc<Mutex<Vec<serde_json::Value>>>,
    pub(super) first: AtomicBool,
}

/// Derive the system-context string from the LlmInput blocks the same way
/// the production OpenAiCompatibleLlm does (serialize_system_context): drop
/// UserMessage/ToolResult blocks and render each remaining block as
/// `## {kind:?}\n{content}`, joined by blank lines. This lets the test
/// compare the two rounds' system context byte-for-byte.
pub(super) fn captured_system(input: &crate::llm::LlmInput) -> String {
    use crate::domain::ContextBlockKind;
    input
        .blocks
        .iter()
        .filter(|b| {
            !matches!(
                b.kind,
                ContextBlockKind::UserMessage | ContextBlockKind::ToolResult
            )
        })
        .map(|b| format!("## {:?}\n{}", b.kind, b.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Serialize the full follow-up transcript (provider turn + result content)
/// so round-2 assertions can inspect the assistant tool call id/operation
/// and the tool-result status/iso/epoch_ms carried in result_content.
pub(super) fn captured_follow_ups(input: &crate::llm::LlmInput) -> serde_json::Value {
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
                    "arguments_json": turn.arguments_json,
                },
                "result_content": fu.result_content,
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}

impl crate::llm::LlmClient for CaptureToolsLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        // Capture the full per-round LlmInput shape: system (derived from
        // blocks), the exact provider_tools list, and the complete follow-up
        // transcript (not just a count) so we can assert on call id, status,
        // iso and epoch_ms in the second round.
        self.captured.lock().unwrap().push(json!({
            "system": captured_system(&input),
            "provider_tools": input.provider_tools,
            "follow_ups": captured_follow_ups(&input),
            "follow_up_count": input.follow_ups.len(),
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

    // ── Full second-round follow-up assertions (not just a count) ──
    // The provider's assistant tool call in the transcript must name the
    // external operation and carry the exact provider tool-call id the
    // script emitted ("cr"), so the role=tool result can match it.
    let fu2 = caps[1]["follow_ups"]
        .as_array()
        .expect("round-2 follow_ups captured as array");
    assert_eq!(fu2.len(), 1, "exactly one follow-up in round 2");
    let assistant_call = &fu2[0]["provider_turn"];
    assert_eq!(
        assistant_call["canonical_operation"], "external.time_now",
        "assistant tool call operation == external.time_now"
    );
    assert_eq!(
        assistant_call["provider_tool_call_id"], "cr",
        "provider tool call id == expected scripted value"
    );

    // The tool result (role=tool) must reference the same call id and carry
    // a succeeded status with the iso + epoch_ms echoed from the harness.
    let result_content = fu2[0]["result_content"]
        .as_str()
        .expect("result_content captured as string");
    assert!(
        result_content.contains("status: succeeded"),
        "tool result status == succeeded; got: {result_content}"
    );
    assert!(
        result_content.contains("iso"),
        "tool result contains iso; got: {result_content}"
    );
    assert!(
        result_content.contains("epoch_ms"),
        "tool result contains epoch_ms; got: {result_content}"
    );

    // The two rounds' system context must be byte-identical (the system
    // blocks are derived from the same pinned snapshot; only the ToolResult
    // block differs, and that is filtered out of the system derivation).
    assert_eq!(
        caps[0]["system"], caps[1]["system"],
        "two rounds' system context must be byte-identical"
    );

    // The external.time_now result content must appear exactly once across
    // the whole run (one tool round, one follow-up).
    let time_now_result_count = caps
        .iter()
        .map(|c| {
            c["follow_ups"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|fu| {
                            fu["result_content"]
                                .as_str()
                                .map(|s| s.contains("epoch_ms"))
                                .unwrap_or(false)
                        })
                        .count()
                })
                .unwrap_or(0)
        })
        .sum::<usize>();
    assert_eq!(
        time_now_result_count, 1,
        "external.time_now result appears exactly once"
    );

    // The Runtime's final output must equal the scripted final reply.
    assert_eq!(
        outcome.output, "done",
        "final Runtime output == scripted final reply"
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
