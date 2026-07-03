//! Helper functions for workspace harness E2E tests.

use crate::config::KernelConfig;
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

pub fn test_config() -> KernelConfig {
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
        extra_allowed_operations: vec![
            "system.status".to_string(),
            "external.workspace_list".to_string(),
            "external.workspace_read".to_string(),
            "external.workspace_write".to_string(),
            "external.workspace_mkdir".to_string(),
            "external.workspace_stat".to_string(),
            "external.workspace_exec".to_string(),
        ],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 30_000,
        harness_artifact_root: std::env::temp_dir()
            .join(format!("ws_e2e_ha_{}", std::process::id())),
        capability_submit_token: None,
        capability_decision_token: None,
    }
}

pub fn start_workspace_harness(
    ws_root: PathBuf,
    delay: Option<Duration>,
) -> Result<(String, Arc<AtomicBool>, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let ep = format!("http://127.0.0.1:{port}/execute");
    let shutdown = Arc::new(AtomicBool::new(false));
    let root = Arc::new(ws_root);
    let sd = Arc::clone(&shutdown);
    thread::spawn(move || {
        for stream in listener.incoming() {
            if sd.load(Ordering::SeqCst) {
                break;
            }
            let root = Arc::clone(&root);
            let d = delay;
            thread::spawn(move || {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => return,
                };
                if let Some(d) = d {
                    thread::sleep(d);
                }
                let mut buf = [0u8; 8192];
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
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(http.as_bytes());
            });
        }
    });
    thread::sleep(Duration::from_millis(100));
    Ok((ep, shutdown, port))
}

fn dispatch(root: &PathBuf, op: &str, args: &Value) -> Value {
    let ws = args
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !ws.is_empty() && ws != "test" {
        return err_resp("unknown_workspace_id");
    }
    match op {
        "external.workspace_list" => list(root, args),
        "external.workspace_read" => read(root, args),
        "external.workspace_write" => write(root, args),
        "external.workspace_mkdir" => mkdir(root, args),
        "external.workspace_stat" => stat(root, args),
        "external.workspace_exec" => exec(root, args),
        _ => err_resp("unknown_operation"),
    }
}

fn list(root: &PathBuf, args: &Value) -> Value {
    let rel = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let dir = root.join(rel);
    if !dir.is_dir() {
        return err_resp("not_found");
    }
    let mut entries = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
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
            entries.push(json!({"name": name, "type": typ, "relative_path": rp}));
        }
    }
    ok_resp(json!({"entries": entries, "entry_count": entries.len()}))
}

fn read(root: &PathBuf, args: &Value) -> Value {
    let rel = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let path = root.join(rel);
    if !path.is_file() {
        return err_resp("not_found");
    }
    let data = std::fs::read(&path).unwrap_or_default();
    let content = String::from_utf8(data).unwrap_or_default();
    ok_resp(json!({"content": content, "truncated": false, "size_bytes": content.len()}))
}

fn write(root: &PathBuf, args: &Value) -> Value {
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
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(content.as_bytes());
            ok_resp(json!({"bytes_written": content.len(), "sha256": hex::encode(h.finalize())}))
        }
        Err(err) => err_resp(&format!("write_failed: {err}")),
    }
}

fn mkdir(root: &PathBuf, args: &Value) -> Value {
    let rel = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let rec = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let path = root.join(rel);
    let r = if rec {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    match r {
        Ok(_) => ok_resp(json!({"created": true})),
        Err(err) => err_resp(&format!("mkdir_failed: {err}")),
    }
}

fn stat(root: &PathBuf, args: &Value) -> Value {
    let rel = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let path = root.join(rel);
    match std::fs::symlink_metadata(&path) {
        Ok(m) => {
            let t = if m.file_type().is_dir() {
                "dir"
            } else {
                "file"
            };
            let mod_at = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            ok_resp(
                json!({"type": t, "size_bytes": m.len(), "modified_at_unix": mod_at, "is_symlink": m.file_type().is_symlink()}),
            )
        }
        Err(err) => err_resp(&format!("stat_failed: {err}")),
    }
}

fn exec(root: &PathBuf, args: &Value) -> Value {
    let prog = args.get("program").and_then(Value::as_str).unwrap_or("");
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
    let timeout_secs = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    let max_out = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(262144) as usize;

    let mut cmd = std::process::Command::new(prog);
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

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            return err_resp(&format!(
                "{}: {err}",
                if err.kind() == std::io::ErrorKind::NotFound {
                    "program_not_found"
                } else {
                    "spawn_failed"
                }
            ));
        }
    };

    let deadline = Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= deadline {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    let stdout = read_stdout(&mut child.stdout);
    let stderr = read_stderr(&mut child.stderr);
    let so = String::from_utf8_lossy(&stdout).to_string();
    let se = String::from_utf8_lossy(&stderr).to_string();

    if timed_out {
        err_resp("exec_timed_out")
    } else {
        ok_resp(
            json!({"exit_code": exit_code,"stdout": so,"stderr": se,"timed_out": false,"stdout_truncated": stdout.len() > max_out, "stderr_truncated": stderr.len() > max_out}),
        )
    }
}

fn read_stdout(s: &mut Option<std::process::ChildStdout>) -> Vec<u8> {
    let mut b = Vec::new();
    if let Some(mut r) = s.take() {
        let _ = r.read_to_end(&mut b);
    }
    b
}
fn read_stderr(s: &mut Option<std::process::ChildStderr>) -> Vec<u8> {
    let mut b = Vec::new();
    if let Some(mut r) = s.take() {
        let _ = r.read_to_end(&mut b);
    }
    b
}

fn ok_resp(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
fn err_resp(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}

pub fn register_manifest(j: &JournalStore, ep: &str, op: &str) -> Result<String> {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "wh-v1".into(),
        artifact_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ep.into(),
        operation_name: op.into(),
        description: format!("ws {op}"),
        input_schema: json!({"type":"object","additionalProperties":true}),
        output_schema: json!({"type":"object","additionalProperties":true}),
        idempotent: false,
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

pub struct SingleToolLlm {
    pub tool_call: Option<Value>,
    pub captured: Arc<Mutex<Vec<Value>>>,
    first: AtomicBool,
}

impl SingleToolLlm {
    pub fn new(op: &str, args: Value) -> Self {
        Self {
            tool_call: Some(json!({"operation": op, "arguments": args})),
            captured: Arc::new(Mutex::new(Vec::new())),
            first: AtomicBool::new(true),
        }
    }
    pub fn captured(&self) -> Arc<Mutex<Vec<Value>>> {
        self.captured.clone()
    }
}

impl crate::llm::LlmClient for SingleToolLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.captured.lock().unwrap().push(json!({
            "provider_tools": input.provider_tools,
            "follow_up_count": input.follow_ups.len(),
        }));
        if self.first.swap(false, Ordering::SeqCst) {
            let tc = self.tool_call.as_ref().expect("tool_call");
            let op = tc["operation"].as_str().expect("op").to_string();
            let args = tc["arguments"].clone();
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                    id: "call_id".into(),
                    operation: op,
                    arguments: args,
                }),
                provider_turn: Some(crate::llm::ProviderToolTurn {
                    endpoint: crate::llm::EndpointChoice::Primary,
                    provider_tool_call_id: "call_id".into(),
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
