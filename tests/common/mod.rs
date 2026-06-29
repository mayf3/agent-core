#![allow(dead_code)]

use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ============================================================
// CaptureServer — local HTTP mock that returns preset responses
// in order and captures each request body.
// ============================================================

pub struct CaptureServer {
    pub port: u16,
    captured: Arc<Mutex<Vec<Value>>>,
    parse_error: Arc<Mutex<Option<String>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CaptureServer {
    pub fn start(responses: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_thread = Arc::clone(&captured);
        let parse_error = Arc::new(Mutex::new(None::<String>));
        let parse_error_thread = Arc::clone(&parse_error);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_thread = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if shutdown_thread.load(Ordering::Relaxed) {
                    break;
                }
                match read_request_json(&mut stream) {
                    Ok(body) => {
                        captured_thread.lock().unwrap().push(body);
                    }
                    Err(error) => {
                        *parse_error_thread.lock().unwrap() = Some(error);
                        break;
                    }
                }
                let body = response.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Self {
            port,
            captured,
            parse_error,
            shutdown,
            handle: Some(handle),
        }
    }

    /// Panic if a parse error was recorded. Call before `requests()` in tests.
    pub fn assert_no_error(&self) {
        let err = self.parse_error.lock().unwrap().take();
        if let Some(msg) = err {
            panic!("CaptureServer parse error: {msg}");
        }
    }

    pub fn parse_error(&self) -> Option<String> {
        self.parse_error.lock().unwrap().clone()
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn requests(&self) -> Vec<Value> {
        self.captured.lock().unwrap().clone()
    }
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request_json(stream: &mut TcpStream) -> std::result::Result<Value, String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .map_err(|e| format!("set_read_timeout failed: {e}"))?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|e| format!("read failed: {e}"))?;
        if read == 0 {
            return Err("connection closed before headers".to_string());
        }
        raw.extend_from_slice(&chunk[..read]);
        if let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };
    let headers = std::str::from_utf8(&raw[..header_end])
        .map_err(|e| format!("invalid UTF-8 in headers: {e}"))?;
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .ok_or_else(|| "missing content-length header".to_string())?;
    let body_start = header_end + 4;
    while raw.len() < body_start + content_length {
        let read = stream
            .read(&mut chunk)
            .map_err(|e| format!("read body failed: {e}"))?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    let body_bytes = &raw[body_start..body_start + content_length.min(raw.len() - body_start)];
    serde_json::from_slice(body_bytes).map_err(|e| format!("JSON parse error: {e}"))
}

/// A successful text-only OpenAI-compatible response.
pub fn text_response(text: &str) -> Value {
    json!({
        "model": "local-stub",
        "choices": [{ "message": { "content": text } }]
    })
}

/// An OpenAI-compatible response with a single tool call.
pub fn tool_call_response(call_id: &str, fn_name: &str, arguments: &str) -> Value {
    json!({
        "model": "local-stub",
        "choices": [{
            "message": {
                "content": "",
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": fn_name, "arguments": arguments }
                }]
            }
        }]
    })
}

/// Fake adapter that returns the configured receipt for any invocation.
pub struct FakeReplyAdapter {
    pub receipt: agent_core_kernel::domain::Receipt,
}

impl agent_core_kernel::adapters::InvocationAdapter for FakeReplyAdapter {
    fn execute(
        &self,
        _invocation: &agent_core_kernel::domain::ApprovedInvocation,
    ) -> anyhow::Result<agent_core_kernel::domain::Receipt> {
        anyhow::Ok(self.receipt.clone())
    }
}

/// Drain all pending outbox dispatches using the given adapter.
pub fn dispatch_all(
    journal: &agent_core_kernel::journal::JournalStore,
    adapter: &impl agent_core_kernel::adapters::InvocationAdapter,
) -> anyhow::Result<()> {
    while agent_core_kernel::runtime::outbox_dispatcher::dispatch_once(journal, adapter)? {}
    Ok(())
}

pub fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
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
        // system.status is part of the dogfood agent's profile, not a
        // channel grant. It's granted here so that in-tests (which also
        // behave as a dogfood agent) can exercise the capability. See
        // KernelConfig::from_cli for the production default.
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
    }
}

pub fn test_session(config: &KernelConfig) -> Session {
    Session {
        id: SessionId("session_test".to_string()),
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Cli,
        conversation_key: "local".to_string(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    }
}

pub fn test_run(config: &KernelConfig, session: &Session) -> Run {
    Run {
        id: RunId("run_test".to_string()),
        session_id: session.id.clone(),
        agent_id: config.agent_id.clone(),
        trigger_event_id: EventId("event_test".to_string()),
        principal: cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: String::new(),
    }
}

pub fn cli_principal() -> RunPrincipal {
    RunPrincipal {
        principal_id: PrincipalId("cli:local".to_string()),
        subject: PrincipalSubject::LocalUser,
        source: PrincipalSource::Cli,
        grants: vec![CapabilityGrant {
            operation: "stdout.send_text".to_string(),
            scope: "current_session".to_string(),
        }],
        requester_id: Some("cli:local".to_string()),
    }
}

pub fn approved_stdout_invocation(
    gateway: &Gateway,
    run: &Run,
    session: &Session,
) -> anyhow::Result<ApprovedInvocation> {
    let snap = agent_core_kernel::registry::snapshot::test_snapshot();
    gateway.approve_invocation(
        InvocationIntent {
            invocation_id: InvocationId("reply:run_test".to_string()),
            run_id: run.id.clone(),
            operation: "stdout.send_text".to_string(),
            arguments: json!({
                "session_id": session.id.0,
                "text": "hello",
            }),
            idempotency_key: Some("stdout-reply:run_test".to_string()),
        },
        run,
        session,
        &snap,
    )
}

pub fn runtime_run(run_id: &RunId, session_id: &SessionId) -> Run {
    Run {
        id: run_id.clone(),
        session_id: session_id.clone(),
        agent_id: AgentId("main".to_string()),
        trigger_event_id: EventId::new(),
        principal: cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: String::new(),
    }
}
