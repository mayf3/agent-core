//! Test helpers for coding_harness_e2e tests.
//!
//! Provides real handler function wrappers and mock LLM stubs.
//! No mock harness implementations — all tests use real production code.

use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Register a harness manifest and enable it in the registry.
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

/// Start a real inline TCP harness responder that delegates to the real
/// workspace handler functions. Returns (endpoint, shutdown_flag).
pub fn start_coding_harness_responder(
    ws_root: std::path::PathBuf,
) -> Result<(String, Arc<AtomicBool>, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let endpoint = format!("http://127.0.0.1:{port}/execute");
    let shutdown = Arc::new(AtomicBool::new(false));
    let root = Arc::new(ws_root);
    let sd = Arc::clone(&shutdown);
    thread::spawn(move || {
        for stream in listener.incoming() {
            if sd.load(Ordering::SeqCst) {
                break;
            }
            let root = Arc::clone(&root);
            thread::spawn(move || {
                let mut stream = match stream {
                    Ok(s) => s,
                    _ => return,
                };
                let mut buf = [0u8; 65536];
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                let parsed: Value = serde_json::from_str(body).unwrap_or(json!({}));
                let op = parsed
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args = parsed.get("arguments").cloned().unwrap_or(json!({}));

                // Use real handler functions from the coding module.
                use crate::harness::coding::config::WorkspacePermission;
                use crate::harness::coding::workspace;

                let default_perm = WorkspacePermission {
                    read: true,
                    write: true,
                    exec: true,
                    zcode: true,
                    ..Default::default()
                };

                let resp = match op {
                    "external.coding_workspace_list" => workspace::handle_list(&root, &args),
                    "external.coding_workspace_read" => workspace::handle_read(&root, &args),
                    "external.coding_workspace_write" => workspace::handle_write(&root, &args),
                    "external.coding_workspace_exec" => {
                        workspace::handle_exec(&root, &args, &default_perm)
                    }
                    "external.coding_task_submit" => {
                        let ws_id = args
                            .get("workspace_id")
                            .and_then(Value::as_str)
                            .unwrap_or("test");
                        let objective = args.get("objective").and_then(Value::as_str).unwrap_or("");
                        let acceptance = args
                            .get("acceptance_criteria")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let backend = args
                            .get("backend")
                            .and_then(Value::as_str)
                            .unwrap_or("fake");
                        crate::harness::coding::tasks::submit_task(
                            ws_id, objective, acceptance, backend,
                        )
                    }
                    "external.coding_task_status" => {
                        let task_id = args.get("task_id").and_then(Value::as_str).unwrap_or("");
                        crate::harness::coding::tasks::get_status(task_id)
                    }
                    _ => {
                        json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"unknown_operation"})
                    }
                };
                let body = serde_json::to_string(&resp).unwrap_or_default();
                let http = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = stream.write_all(http.as_bytes());
            });
        }
    });
    thread::sleep(Duration::from_millis(100));
    Ok((endpoint, shutdown, port))
}

/// Start an inline calculator harness responder.
pub fn start_calculator_responder() -> Result<(String, Arc<AtomicBool>, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let endpoint = format!("http://127.0.0.1:{port}/execute");
    let shutdown = Arc::new(AtomicBool::new(false));
    let sd = Arc::clone(&shutdown);
    thread::spawn(move || {
        for stream in listener.incoming() {
            if sd.load(Ordering::SeqCst) {
                break;
            }
            thread::spawn(move || {
                let mut stream = match stream {
                    Ok(s) => s,
                    _ => return,
                };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                let parsed: Value = serde_json::from_str(body).unwrap_or(json!({}));
                let args = parsed.get("arguments").cloned().unwrap_or(json!({}));
                let op = args.get("operation").and_then(Value::as_str).unwrap_or("");
                let a = args.get("a").and_then(Value::as_f64).unwrap_or(0.0);
                let b = args.get("b").and_then(Value::as_f64).unwrap_or(0.0);
                let (ok, result, error) = match op {
                    "add" => (true, Some(a + b), None),
                    "subtract" => (true, Some(a - b), None),
                    "multiply" => (true, Some(a * b), None),
                    "divide" => {
                        if b == 0.0 {
                            (false, None, Some("divide_by_zero"))
                        } else {
                            (true, Some(a / b), None)
                        }
                    }
                    _ => (false, None, Some("unsupported_operation")),
                };
                let resp = if ok {
                    json!({"protocol_version":"external-harness-v1","ok":true,"result":{"result":result.unwrap()}})
                } else {
                    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":error.unwrap()})
                };
                let body = serde_json::to_string(&resp).unwrap_or_default();
                let http = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = stream.write_all(http.as_bytes());
            });
        }
    });
    thread::sleep(Duration::from_millis(100));
    Ok((endpoint, shutdown, port))
}

/// A mock LLM that returns a single scripted tool call on round 1, done on round 2.
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
