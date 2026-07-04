//! Test helpers for coding_harness_e2e — TCP responder + mock kernel API.

use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_submit_proposal;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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

/// Start a mock Kernel Proposal API endpoint.
/// Handles POST /v1/capability-change-proposals and returns real proposal_id.
pub fn start_mock_kernel_api(
    journal: &'static JournalStore,
    gateway: &'static Gateway,
    agent_id: &'static AgentId,
    submit_token: &'static str,
) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = Arc::new(AtomicBool::new(false));
    let sd = shutdown.clone();
    let tok = submit_token.to_string();
    thread::spawn(move || {
        for stream in listener.incoming() {
            if sd.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(mut s) = stream {
                let mut buf = [0u8; 65536];
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => continue,
                    Ok(n) => {
                        let req = String::from_utf8_lossy(&buf[..n]);
                        // Extract body after headers.
                        let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                        let parsed: Value = match serde_json::from_str(body) {
                            Ok(v) => v,
                            Err(_) => {
                                let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 30\r\nConnection: close\r\n\r\n{\"error\":\"invalid_json\"}";
                                let _ = s.write_all(resp.as_bytes());
                                continue;
                            }
                        };
                        // Verify Bearer token.
                        let auth = format!("Bearer {}", tok);
                        if !req.contains(&auth) {
                            let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 30\r\nConnection: close\r\n\r\n{\"error\":\"unauthorized\"}";
                            let _ = s.write_all(resp.as_bytes());
                            continue;
                        }
                        // Call the real proposal handler.
                        match handle_submit_proposal(
                            journal,
                            gateway,
                            &parsed,
                            "coding_harness",
                            agent_id,
                        ) {
                            Ok(response) => {
                                let body = serde_json::to_string(&response).unwrap_or_default();
                                let resp = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(), body
                                );
                                let _ = s.write_all(resp.as_bytes());
                            }
                            Err(e) => {
                                let err_body = json!({"ok":false,"error":format!("{e}")});
                                let body = serde_json::to_string(&err_body).unwrap_or_default();
                                let resp = format!(
                                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(), body
                                );
                                let _ = s.write_all(resp.as_bytes());
                            }
                        }
                    }
                }
            }
        }
    });
    thread::sleep(Duration::from_millis(100));
    (port, shutdown)
}

pub struct SingleToolLlm {
    tc: Option<Value>,
    pub captured: Arc<Mutex<Vec<Value>>>,
    first: AtomicBool,
}

impl SingleToolLlm {
    pub fn new(operation: &str, arguments: Value) -> Self {
        Self {
            tc: Some(json!({"operation":operation,"arguments":arguments})),
            captured: Arc::new(Mutex::new(Vec::new())),
            first: AtomicBool::new(true),
        }
    }
}

impl crate::llm::LlmClient for SingleToolLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.captured.lock().unwrap().push(
            json!({"provider_tools":input.provider_tools,"follow_up_count":input.follow_ups.len()}),
        );
        if self.first.swap(false, Ordering::SeqCst) {
            let tc = self.tc.as_ref().expect("tc");
            let op = tc["operation"].as_str().expect("op").to_string();
            let args = tc["arguments"].clone();
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                    id: "c".into(),
                    operation: op,
                    arguments: args,
                }),
                provider_turn: Some(crate::llm::ProviderToolTurn {
                    endpoint: crate::llm::EndpointChoice::Primary,
                    provider_tool_call_id: "c".into(),
                    wire_name: tc["operation"].as_str().unwrap_or("").to_string(),
                    canonical_operation: tc["operation"].as_str().unwrap_or("").to_string(),
                    arguments_json: tc["arguments"].to_string(),
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
