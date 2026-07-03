//! Mock helpers for coding_harness_e2e tests.

use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub fn register_manifest(
    j: &JournalStore,
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
    Ok(mid)
}

pub fn enable_op(j: &JournalStore, g: &Gateway, mid: &str) -> Result<()> {
    j.enable_harness(&g.approve_harness_change(HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: mid.into(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    })?)?;
    Ok(())
}

// ── Mock TCP coding harness ──

pub fn start_mock_harness(ws_root: PathBuf) -> Result<(String, Arc<AtomicBool>, u16)> {
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
                let mut buf = [0u8; 16384];
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
                let resp = dispatch(&root, op, &args);
                let body = serde_json::to_string(&resp).unwrap_or_default();
                let http = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = stream.write_all(http.as_bytes());
            });
        }
    });
    thread::sleep(Duration::from_millis(100));
    Ok((endpoint, shutdown, port))
}

fn dispatch(root: &PathBuf, op: &str, args: &Value) -> Value {
    match op {
        "external.coding_workspace_list" => mock_list(root, args),
        "external.coding_workspace_read" => mock_read(root, args),
        "external.coding_workspace_write" => mock_write(root, args),
        "external.coding_workspace_exec" => mock_exec(root, args),
        "external.coding_task_submit" => {
            json!({"protocol_version":"external-harness-v1","ok":true,"result":{"task_id":"task_1","status":"queued"}})
        }
        "external.coding_task_status" => {
            json!({"protocol_version":"external-harness-v1","ok":true,"result":{"task_id":"task_1","status":"succeeded","summary":"ok"}})
        }
        "external.coding_capability_propose" => {
            json!({"protocol_version":"external-harness-v1","ok":true,"result":{"proposal":{"operation_name":"external.calculator","artifact_digest":"sha256:abc","manifest_size":100},"status":"pending_proposal"}})
        }
        _ => {
            json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"unknown_operation"})
        }
    }
}

fn mock_list(root: &PathBuf, args: &Value) -> Value {
    let rel = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let dir = root.join(rel);
    if !dir.is_dir() {
        return json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"not_found"});
    }
    use std::fs;
    let mut entries = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let typ = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                "dir"
            } else {
                "file"
            };
            let rp = entry
                .path()
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            entries.push(json!({"name":name,"type":typ,"relative_path":rp}));
        }
    }
    json!({"protocol_version":"external-harness-v1","ok":true,"result":{"entries":entries,"entry_count":entries.len()}})
}

fn mock_read(root: &PathBuf, args: &Value) -> Value {
    let rel = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let path = root.join(rel);
    if !path.is_file() {
        return json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"not_found"});
    }
    let data = std::fs::read(&path).unwrap_or_default();
    let content = String::from_utf8_lossy(&data).to_string();
    json!({"protocol_version":"external-harness-v1","ok":true,"result":{"content":content,"truncated":false,"size_bytes":data.len()}})
}

fn mock_write(root: &PathBuf, args: &Value) -> Value {
    let rel = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");
    let path = root.join(rel);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    match std::fs::write(&path, content) {
        Ok(_) => {
            json!({"protocol_version":"external-harness-v1","ok":true,"result":{"bytes_written":content.len()}})
        }
        Err(e) => {
            json!({"protocol_version":"external-harness-v1","ok":false,"error_code":format!("{e}")})
        }
    }
}

fn mock_exec(root: &PathBuf, args: &Value) -> Value {
    let program = args.get("program").and_then(Value::as_str).unwrap_or("");
    let cmd_args: Vec<&str> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let cwd = root.join(
        args.get("relative_cwd")
            .and_then(Value::as_str)
            .unwrap_or("."),
    );
    let mut cmd = std::process::Command::new(program);
    cmd.args(&cmd_args).current_dir(&cwd);
    cmd.env_clear();
    if let Some(v) = std::env::var_os("PATH") {
        cmd.env("PATH", v);
    }
    if let Some(v) = std::env::var_os("HOME") {
        cmd.env("HOME", v);
    }
    if let Some(v) = std::env::var_os("TMPDIR") {
        cmd.env("TMPDIR", v);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return json!({"protocol_version":"external-harness-v1","ok":false,"error_code":format!("spawn_failed: {e}")})
        }
    };
    json!({"protocol_version":"external-harness-v1","ok":true,"result":{"exit_code":output.status.code().unwrap_or(-1),"stdout":String::from_utf8_lossy(&output.stdout).to_string(),"stderr":String::from_utf8_lossy(&output.stderr).to_string(),"timed_out":false}})
}

// ── Mock calculator harness ──

pub fn start_calculator_harness() -> Result<(String, Arc<AtomicBool>, u16)> {
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

// ── Mock LLM ──

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
