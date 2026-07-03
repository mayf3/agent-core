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
use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ── Shared test config ──

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

// ── Mock TCP workspace harness ──

pub fn start_workspace_harness(
    workspace_root: PathBuf,
    response_delay: Option<Duration>,
) -> Result<(String, Arc<AtomicBool>, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let endpoint = format!("http://127.0.0.1:{port}/execute");
    let shutdown = Arc::new(AtomicBool::new(false));
    let root = Arc::new(workspace_root);
    let shutdown_handle = Arc::clone(&shutdown);

    thread::spawn(move || {
        for stream in listener.incoming() {
            if shutdown_handle.load(Ordering::SeqCst) {
                break;
            }
            let root = Arc::clone(&root);
            let delay = response_delay;
            thread::spawn(move || {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => return,
                };
                if let Some(d) = delay {
                    thread::sleep(d);
                }
                let mut buf = [0u8; 8192];
                let n = match stream.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let request = String::from_utf8_lossy(&buf[..n]);
                let body = match request.split("\r\n\r\n").nth(1) {
                    Some(b) => b,
                    None => return,
                };
                let parsed: Value = match serde_json::from_str(body) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = write_http(&mut stream, 400, "invalid_json");
                        return;
                    }
                };
                let operation = parsed.get("operation").and_then(Value::as_str).unwrap_or("");
                let args = parsed.get("arguments").cloned().unwrap_or(json!({}));
                let response = dispatch_mock_op(&root, operation, &args);
                let body_str = serde_json::to_string(&response).unwrap_or_default();
                let _ = write_http(&mut stream, 200, &body_str);
            });
        }
    });

    thread::sleep(Duration::from_millis(100));
    Ok((endpoint, shutdown, port))
}

fn write_http(stream: &mut dyn std::io::Write, status: u16, body: &str) -> std::io::Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    stream.write_all(response.as_bytes())
}

fn dispatch_mock_op(root: &PathBuf, operation: &str, args: &Value) -> Value {
    let ws_id = args.get("workspace_id").and_then(Value::as_str).unwrap_or("");
    if !ws_id.is_empty() && ws_id != "test" {
        return json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"unknown_workspace_id"});
    }
    match operation {
        "external.workspace_list" => mock_list(root, args),
        "external.workspace_read" => mock_read(root, args),
        "external.workspace_write" => mock_write(root, args),
        "external.workspace_mkdir" => mock_mkdir(root, args),
        "external.workspace_stat" => mock_stat(root, args),
        "external.workspace_exec" => mock_exec(root, args),
        _ => json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"unknown_operation"}),
    }
}

fn mock_list(root: &PathBuf, args: &Value) -> Value {
    let relative = args.get("relative_path").and_then(Value::as_str).unwrap_or(".");
    let dir = root.join(relative);
    if !dir.is_dir() {
        return json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"not_found"});
    }
    let mut entries = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let typ = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                "dir"
            } else {
                "file"
            };
            let rel_path = entry.path().strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            entries.push(json!({"name": name, "type": typ, "relative_path": rel_path}));
        }
    }
    json!({"protocol_version":"external-harness-v1","ok":true,"result":{"entries":entries,"entry_count":entries.len()}})
}

fn mock_read(root: &PathBuf, args: &Value) -> Value {
    let relative = args.get("relative_path").and_then(Value::as_str).unwrap_or("");
    let path = root.join(relative);
    if !path.is_file() {
        return json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"not_found"});
    }
    let data = std::fs::read(&path).unwrap_or_default();
    let content = String::from_utf8(data).unwrap_or_default();
    json!({"protocol_version":"external-harness-v1","ok":true,"result":{"content":content,"truncated":false,"size_bytes":content.len()}})
}

fn mock_write(root: &PathBuf, args: &Value) -> Value {
    let relative = args.get("relative_path").and_then(Value::as_str).unwrap_or("");
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, content) {
        Ok(_) => {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(content.as_bytes());
            let sha256 = hex::encode(hasher.finalize());
            json!({"protocol_version":"external-harness-v1","ok":true,"result":{"bytes_written":content.len(),"sha256":sha256}})
        }
        Err(e) => json!({"protocol_version":"external-harness-v1","ok":false,"error_code":format!("write_failed: {e}")}),
    }
}

fn mock_mkdir(root: &PathBuf, args: &Value) -> Value {
    let relative = args.get("relative_path").and_then(Value::as_str).unwrap_or("");
    let recursive = args.get("recursive").and_then(Value::as_bool).unwrap_or(false);
    let path = root.join(relative);
    let result = if recursive { std::fs::create_dir_all(&path) } else { std::fs::create_dir(&path) };
    match result {
        Ok(_) => json!({"protocol_version":"external-harness-v1","ok":true,"result":{"created":true}}),
        Err(e) => json!({"protocol_version":"external-harness-v1","ok":false,"error_code":format!("mkdir_failed: {e}")}),
    }
}

fn mock_stat(root: &PathBuf, args: &Value) -> Value {
    let relative = args.get("relative_path").and_then(Value::as_str).unwrap_or("");
    let path = root.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            let entry_type = if meta.file_type().is_dir() { "dir" } else { "file" };
            let modified = meta.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            json!({"protocol_version":"external-harness-v1","ok":true,"result":{"type":entry_type,"size_bytes":meta.len(),"modified_at_unix":modified,"is_symlink":meta.file_type().is_symlink()}})
        }
        Err(e) => json!({"protocol_version":"external-harness-v1","ok":false,"error_code":format!("stat_failed: {e}")}),
    }
}

fn mock_exec(root: &PathBuf, args: &Value) -> Value {
    let program = args.get("program").and_then(Value::as_str).unwrap_or("");
    let cmd_args: Vec<&str> = args.get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let cwd = root.join(args.get("relative_cwd").and_then(Value::as_str).unwrap_or("."));
    let timeout_secs = args.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(30);
    let max_output = args.get("max_output_bytes").and_then(Value::as_u64).unwrap_or(262144) as usize;

    let mut cmd = std::process::Command::new(program);
    cmd.args(&cmd_args);
    cmd.current_dir(&cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let code = if e.kind() == std::io::ErrorKind::NotFound { "program_not_found" } else { "spawn_failed" };
            return json!({"protocol_version":"external-harness-v1","ok":false,"error_code":format!("{code}: {e}")});
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
    let stdout_str = String::from_utf8_lossy(&read_stdout(&mut child.stdout)).to_string();
    let stderr_str = String::from_utf8_lossy(&read_stderr(&mut child.stderr)).to_string();

    json!({"protocol_version":"external-harness-v1","ok":true,"result":{"exit_code":exit_code,"stdout":stdout_str,"stderr":stderr_str,"timed_out":timed_out,"stdout_truncated":stdout_str.len()>max_output,"stderr_truncated":stderr_str.len()>max_output}})
}

fn read_stdout(stream: &mut Option<std::process::ChildStdout>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut r) = stream.take() {
        let _ = r.read_to_end(&mut buf);
    }
    buf
}

fn read_stderr(stream: &mut Option<std::process::ChildStderr>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut r) = stream.take() {
        let _ = r.read_to_end(&mut buf);
    }
    buf
}

// ── Helpers ──

pub fn register_workspace_manifest(
    j: &JournalStore,
    ep: &str,
    operation_name: &str,
    input_schema: Value,
    output_schema: Value,
) -> Result<String> {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "workspace-harness-v1".into(),
        artifact_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ep.into(),
        operation_name: operation_name.into(),
        description: format!("Coding workspace {operation_name}"),
        input_schema,
        output_schema,
        idempotent: false,
        created_at: Utc::now(),
    };
    let mid = m.compute_manifest_id()?;
    m.manifest_id = mid.clone();
    j.register_harness_manifest(&m)?;
    Ok(mid)
}

pub fn enable_workspace_operation(j: &JournalStore, g: &Gateway, manifest_id: &str) -> Result<()> {
    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: manifest_id.into(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    };
    j.enable_harness(&g.approve_harness_change(intent)?)?;
    Ok(())
}

/// Mock LLM that returns a single hardcoded tool call.
pub struct SingleToolLlm {
    pub tool_call: Option<Value>,
    pub captured: Arc<Mutex<Vec<Value>>>,
    first: AtomicBool,
}

impl SingleToolLlm {
    pub fn new(operation: &str, arguments: Value) -> Self {
        Self {
            tool_call: Some(json!({"operation": operation, "arguments": arguments})),
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
            let operation = tc["operation"].as_str().expect("operation").to_string();
            let arguments = tc["arguments"].clone();
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                    id: "call_id".into(),
                    operation,
                    arguments,
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
