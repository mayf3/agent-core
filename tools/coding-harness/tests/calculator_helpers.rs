//! Shared test helpers for calculator vertical E2E.

use agent_core_kernel::capabilities::store::ContentStore;
use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use agent_core_kernel::harness::manifest::HarnessManifest;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use agent_core_kernel::runtime::{Runtime, RuntimeOutcome};
use agent_core_kernel::server::capability_routes;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

pub struct SingleToolLlm {
    tc: Option<Value>,
    first: AtomicBool,
}

impl SingleToolLlm {
    pub fn new(operation: &str, arguments: Value) -> Self {
        Self {
            tc: Some(json!({"operation": operation, "arguments": arguments})),
            first: AtomicBool::new(true),
        }
    }
}

impl LlmClient for SingleToolLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let tc = self.tc.as_ref().expect("tc");
            let op = tc["operation"].as_str().expect("op").to_string();
            let args = tc["arguments"].clone();
            Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "c".into(),
                    operation: op,
                    arguments: args,
                }),
                provider_turn: Some(agent_core_kernel::llm::ProviderToolTurn {
                    endpoint: agent_core_kernel::llm::EndpointChoice::Primary,
                    provider_tool_call_id: "c".into(),
                    wire_name: tc["operation"].as_str().unwrap_or("").to_string(),
                    canonical_operation: tc["operation"].as_str().unwrap_or("").to_string(),
                    arguments_json: tc["arguments"].to_string(),
                    reasoning_content: None,
                }),
            })
        } else {
            Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "done".into(),
                journal_payload: json!({"s":"ok","c":"done"}),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

pub fn register_and_enable(
    j: &JournalStore,
    g: &Gateway,
    ep: &str,
    name: &str,
    input: Value,
    output: Value,
) -> Result<String> {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "h".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ep.into(),
        operation_name: name.into(),
        description: name.into(),
        input_schema: input,
        output_schema: output,
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

pub fn make_event(
    g: &Gateway,
    j: &JournalStore,
    text: &str,
    extra_ops: &[&str],
    agent_id: &AgentId,
) -> Result<ValidatedEvent> {
    let envelope = g.cli_ingress(text.into())?;
    let event_id = EventId::new();
    let source = "cli";
    let mut grants = vec![
        CapabilityGrant {
            operation: "stdout.send_text".into(),
            scope: "current_session".into(),
        },
        CapabilityGrant {
            operation: "session.recall_recent".into(),
            scope: "current_session".into(),
        },
    ];
    for op in extra_ops {
        grants.push(CapabilityGrant {
            operation: op.to_string(),
            scope: "current_session".into(),
        });
    }
    let event = ValidatedEvent {
        event_id: event_id.clone(),
        source: EventSource::Cli,
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:local".to_string()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants,
            requester_id: Some("cli:local".to_string()),
        },
        session_target: SessionTarget {
            agent_id: agent_id.clone(),
            channel: ChannelKind::Cli,
            conversation_key: "local".to_string(),
        },
        payload: RuntimeEventPayload::UserMessage {
            text: text.to_string(),
            message_id: None,
            chat_id: None,
        },
        dedupe_key: format!("{source}:{}", envelope.external_event_id),
        occurred_at: envelope.received_at,
    };
    j.accept_ingress_with_worker_job(
        &event,
        json!({"source": source, "event_id": event_id.0.clone()}),
    )?;
    Ok(event)
}

pub fn deliver_tool(
    j: &JournalStore,
    g: &Gateway,
    config: &KernelConfig,
    operation: &str,
    arguments: Value,
) -> Result<RuntimeOutcome> {
    let ev = make_event(g, j, "t", &[operation], &config.agent_id)?;
    Runtime::new(config.clone(), SingleToolLlm::new(operation, arguments)).deliver(j, g, ev)
}

pub fn kcfg(artifact_root: &PathBuf) -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 0,
        connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_coding_owner_id: None,
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
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 30_000,
        harness_artifact_root: artifact_root.clone(),
        max_tool_rounds: 12,
        capability_submit_token: Some("test-submit-token".into()),
        capability_decision_token: Some("test-decision-token".into()),
    }
}

pub fn write_http(stream: &mut TcpStream, status: u16, body: Value) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let payload = serde_json::to_string(&body)?;
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(), payload
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

pub fn parse_http_request(req: &str) -> (String, String, String, String) {
    let mut lines = req.lines();
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    let method = parts.first().unwrap_or(&"").to_string();
    let path = parts.get(1).unwrap_or(&"").to_string();
    let mut bearer = String::new();
    let mut content_length: usize = 0;
    for line in lines.clone() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") {
                bearer = value
                    .trim()
                    .strip_prefix("Bearer ")
                    .unwrap_or("")
                    .trim()
                    .to_string();
            }
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let body = if content_length > 0 {
        let all_lines: Vec<&str> = req.split("\r\n\r\n").collect();
        if all_lines.len() > 1 {
            all_lines[1].to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    (path, method, bearer, body)
}

pub fn start_kernel_api(
    journal: &'static JournalStore,
    gateway: &'static Gateway,
    config: &'static KernelConfig,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut s) = stream {
                let mut all = Vec::new();
                let mut tmp = [0u8; 65536];
                loop {
                    match s.set_read_timeout(Some(Duration::from_millis(200))) {
                        Ok(()) => match s.read(&mut tmp) {
                            Ok(0) => break,
                            Ok(n) => all.extend_from_slice(&tmp[..n]),
                            Err(_) => break,
                        },
                        Err(_) => break,
                    }
                }
                let req = String::from_utf8_lossy(&all);
                let (path, method, bearer, body) = parse_http_request(&req);
                if method != "POST" || !path.starts_with("/v1/") {
                    let _ = write_http(&mut s, 404, json!({"error":"not_found"}));
                    continue;
                }
                if path == "/v1/capability-change-proposals" {
                    let expected = config.capability_submit_token.as_deref().unwrap_or("");
                    if bearer != expected {
                        let _ = write_http(&mut s, 401, json!({"error":"unauthorized"}));
                        continue;
                    }
                    let parsed: Value = match serde_json::from_str(&body) {
                        Ok(v) => v,
                        Err(_) => {
                            let _ = write_http(&mut s, 400, json!({"error":"invalid_json"}));
                            continue;
                        }
                    };
                    match capability_routes::handle_submit_proposal(
                        journal,
                        gateway,
                        &parsed,
                        "coding_harness",
                        &config.agent_id,
                    ) {
                        Ok(resp) => {
                            let v = serde_json::to_value(&resp).unwrap_or_default();
                            let _ = write_http(&mut s, 200, v);
                        }
                        Err(e) => {
                            let _ = write_http(&mut s, 400, json!({"error": e.to_string()}));
                        }
                    }
                } else if let Some(pid) = path
                    .strip_prefix("/v1/capability-change-proposals/")
                    .and_then(|s| s.strip_suffix("/decision"))
                {
                    let expected = config.capability_decision_token.as_deref().unwrap_or("");
                    if bearer != expected {
                        let _ = write_http(&mut s, 401, json!({"error":"unauthorized"}));
                        continue;
                    }
                    let parsed: Value = match serde_json::from_str(&body) {
                        Ok(v) => v,
                        Err(_) => {
                            let _ = write_http(&mut s, 400, json!({"error":"invalid_json"}));
                            continue;
                        }
                    };
                    let store = ContentStore::new(config.harness_artifact_root.clone());
                    match capability_routes::handle_decision(
                        journal,
                        gateway,
                        &store,
                        pid,
                        &parsed,
                        "approval_workflow",
                        &config.agent_id,
                    ) {
                        Ok(v) => {
                            let _ = write_http(&mut s, 200, v);
                        }
                        Err(e) => {
                            let _ = write_http(&mut s, 400, json!({"error": e.to_string()}));
                        }
                    }
                } else {
                    let _ = write_http(&mut s, 404, json!({"error":"not_found"}));
                }
            }
        }
    });
    thread::sleep(Duration::from_millis(100));
    port
}
