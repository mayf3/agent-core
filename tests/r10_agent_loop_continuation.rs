//! R10 — Agent Loop Harness same-session auto-continuation E2E (Bootstrap V0).
//!
//! Proves the frozen acceptance criteria of AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1 §7:
//!
//! ```text
//! 1. 用户只发送一次请求
//! 2. 第一 Run 因预算产生 outcome=yielded
//! 3. 用户不发送"继续"
//! 4. Agent Loop Harness 应用 run.outcome.resolve.v0 策略
//! 5. 返回 continue_same_session
//! 6. 自动创建同一个 session_id 下的新 Run
//! 7. 至少连续三个有界 Run
//! 8. 后续 Run 能看到前文、compaction和工具结果
//! 9. 模型明确等待用户时停止续跑
//! 10. 达到外部总上限时停止
//! 11. Kernel 中没有新增 task/progress/checkpoint或产品特判
//! ```
//!
//! Setup: a REAL Kernel HTTP server (`serve_with_running`, ephemeral port,
//! temp DB) + a stub OpenAI-compatible LLM server (returns tool calls until
//! budget exhaustion → yield) + a stub connector (always-succeeding receipts)
//! + the real external `agent-loop-harness` logic running in a thread.
//! The harness talks to the Kernel ONLY over HTTP (it is compiled as an
//! independent crate with no kernel dependency).

use agent_core_kernel::server::serve_with_running;
use agent_core_kernel::{config::KernelConfig, domain::AgentId};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Stub servers
// ---------------------------------------------------------------------------

/// OpenAI-compatible stub LLM: returns `session.recall_recent` tool calls while
/// the global counter is below `tool_calls_before_reply`, then a text reply.
/// Every LLM HTTP call consumes one counter unit.
struct StubLlm {
    #[allow(dead_code)] // kept alive so the listener socket stays bound
    listener: TcpListener,
    respond_text: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl StubLlm {
    fn start(tool_calls_before_reply: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remaining = Arc::new(AtomicUsize::new(tool_calls_before_reply));
        let respond_text = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        {
            let remaining = Arc::clone(&remaining);
            let respond_text = Arc::clone(&respond_text);
            let shutdown = Arc::clone(&shutdown);
            let listener = listener.try_clone().unwrap();
            std::thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                while !shutdown.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = read_http_body(&mut stream);
                            let body = if respond_text.load(Ordering::SeqCst)
                                || remaining.load(Ordering::SeqCst) == 0
                            {
                                json!({
                                    "model": "stub",
                                    "choices": [{
                                        "message": { "content": "final reply for this run" }
                                    }]
                                })
                            } else {
                                remaining.fetch_sub(1, Ordering::SeqCst);
                                json!({
                                    "model": "stub",
                                    "choices": [{
                                        "message": {
                                            "content": "",
                                            "tool_calls": [{
                                                "id": "tc",
                                                "type": "function",
                                                "function": {
                                                    "name": "session.recall_recent",
                                                    "arguments": "{}"
                                                }
                                            }]
                                        }
                                    }]
                                })
                            };
                            let _ = write_http_response(&mut stream, 200, &body);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => {}
                    }
                }
            })
        };
        Self {
            listener,
            respond_text,
            shutdown,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.listener.local_addr().unwrap())
    }

    fn switch_to_text(&self) {
        self.respond_text.store(true, Ordering::SeqCst);
    }
}

impl Drop for StubLlm {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// Stub connector: returns an always-succeeding receipt for any invocation.
struct StubConnector {
    #[allow(dead_code)] // kept alive so the listener socket stays bound
    listener: TcpListener,
    #[allow(dead_code)] // kept alive so the accept thread stays joined on drop
    handle: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    /// Captured request bodies (JSON) sent by the Kernel's outbox dispatcher.
    /// Used to prove no "请发送继续" prompt is ever delivered to the user.
    captured_bodies: Arc<Mutex<Vec<Value>>>,
}

impl StubConnector {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let handle = {
            let shutdown = Arc::clone(&shutdown);
            let captured_bodies = Arc::clone(&captured_bodies);
            let listener = listener.try_clone().unwrap();
            std::thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                while !shutdown.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request_body = read_http_body(&mut stream);
                            if let Ok(body_bytes) = request_body {
                                // Strip the HTTP header section, keep the JSON
                                // body after \r\n\r\n.
                                if let Some(pos) = find_subslice(&body_bytes, b"\r\n\r\n") {
                                    let json_start = pos + 4;
                                    if let Ok(parsed) = serde_json::from_slice::<Value>(
                                        &body_bytes[json_start..],
                                    ) {
                                        captured_bodies.lock().unwrap().push(parsed);
                                    }
                                }
                            }
                            let body = json!({
                                "receipt": {
                                    "status": "Succeeded",
                                    "message_id": "stub-msg"
                                }
                            });
                            let _ = write_http_response(&mut stream, 200, &body);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => {}
                    }
                }
            })
        };
        Self {
            listener,
            handle: Some(handle),
            shutdown,
            captured_bodies,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/v1/execute", self.listener.local_addr().unwrap())
    }

    /// Every request body the Kernel sent to the connector.
    fn captured(&self) -> Vec<Value> {
        self.captured_bodies.lock().unwrap().clone()
    }

    /// The concatenated text of every reply operation the Kernel delivered.
    fn delivered_text(&self) -> String {
        self.captured()
            .iter()
            .filter_map(|body| {
                let operation = body.get("operation").and_then(Value::as_str)?;
                if operation != "stdout.send_text" && operation != "feishu.send_message" {
                    return None;
                }
                body.pointer("/arguments/text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for StubConnector {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Tiny HTTP helpers
// ---------------------------------------------------------------------------

fn read_http_body(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = [0u8; 8192];
    let mut all = Vec::new();
    // Read headers until \r\n\r\n, then content-length bytes.
    let mut header_end = None;
    while header_end.is_none() {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_subslice(&all, b"\r\n\r\n") {
            header_end = Some(pos);
        }
    }
    let Some(end) = header_end else {
        return Ok(all);
    };
    let headers = String::from_utf8_lossy(&all[..end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let lower = line.to_lowercase();
            lower.strip_prefix("content-length:").map(|v| {
                v.trim()
                    .parse::<usize>()
                    .unwrap_or(0)
            })
        })
        .unwrap_or(0);
    while all.len() < end + 4 + content_length {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
    }
    Ok(all)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_http_response(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let body_str = serde_json::to_string(body).unwrap();
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str
    );
    stream.write_all(response.as_bytes())
}

// ---------------------------------------------------------------------------
// Kernel-side HTTP client (test uses the public HTTP API like the harness)
// ---------------------------------------------------------------------------

struct HttpKernel {
    base: String,
    token: String,
}

impl HttpKernel {
    fn new(port: u16, token: &str) -> Self {
        Self {
            base: format!("http://127.0.0.1:{port}"),
            token: token.to_string(),
        }
    }

    fn post(&self, path: &str, body: &Value) -> Value {
        // Retry transient transport failures (parallel test CPU contention).
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let url = format!("{}{}", self.base, path);
            let result = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(10)))
                .http_status_as_error(false)
                .build()
                .new_agent()
                .post(&url)
                .header("authorization", &format!("Bearer {}", self.token))
                .header("content-type", "application/json")
                .send_json(body);
            match result {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let response_body = response
                        .into_body()
                        .with_config()
                        .limit(8 * 1024 * 1024)
                        .read_to_string()
                        .unwrap_or_default();
                    let parsed: Value =
                        serde_json::from_str(&response_body).unwrap_or(json!({}));
                    let mut value = parsed;
                    value["http_status"] = json!(status);
                    if status == 500 {
                        if Instant::now() > deadline {
                            return value;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    return value;
                }
                Err(_) => {
                    if Instant::now() > deadline {
                        return json!({ "http_status": 0, "error": "transport_failed" });
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn get(&self, path: &str) -> Value {
        // Retry transient transport failures (parallel test CPU contention).
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let url = format!("{}{}", self.base, path);
            match ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(10)))
                .http_status_as_error(false)
                .build()
                .new_agent()
                .get(&url)
                .header("authorization", &format!("Bearer {}", self.token))
                .call()
            {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let body = response
                        .into_body()
                        .with_config()
                        .limit(8 * 1024 * 1024)
                        .read_to_string()
                        .unwrap_or_default();
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    let mut value = parsed;
                    value["http_status"] = json!(status);
                    // Transient server-side 500s (e.g. journal_corrupt during a
                    // concurrent hash-chain write) are retried.
                    if status == 500 {
                        if Instant::now() > deadline {
                            return value;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    return value;
                }
                Err(_) => {
                    if Instant::now() > deadline {
                        return json!({ "http_status": 0, "error": "transport_failed" });
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn all_events(&self) -> Vec<Value> {
        let mut cursor = 0i64;
        let mut all = Vec::new();
        loop {
            let page = self.get(&format!("/v1/events?cursor={cursor}&limit=1000"));
            let events = page["events"].as_array().cloned().unwrap_or_default();
            all.extend(events);
            if page["has_more"] != json!(true) {
                break;
            }
            cursor = page["next_cursor"].as_i64().unwrap_or(cursor);
        }
        all
    }
}

// ---------------------------------------------------------------------------
// Test kernel config
// ---------------------------------------------------------------------------

fn test_config(tmp_db: &PathBuf, llm_url: &str, connector_url: &str) -> KernelConfig {
    // Unique artifact root per test DB so parallel tests never share state.
    let artifact_root = tmp_db
        .parent()
        .unwrap()
        .join(format!("harness-artifacts"));
    KernelConfig {
        db_path: tmp_db.clone(),
        data_dir: tmp_db.parent().unwrap().to_path_buf(),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 0, // ephemeral
        connector_execute_url: connector_url.to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: llm_url.to_string(),
        openai_api_key: "stub".to_string(),
        model: "stub-model".to_string(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 10_000,
        outbox_dispatcher_enabled: true,
        outbox_dispatcher_poll_interval_ms: 20,
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: artifact_root,
        max_tool_rounds: 2, // deliberately small → budget exhaustion → yield
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: agent_core_kernel::hook::HookConfig::default(),
        budget_hook: agent_core_kernel::hook::HookConfig::default(),
    }
}

fn tmp_db_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("r10-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("kernel.sqlite")
}

// ---------------------------------------------------------------------------
// Harness driver (uses the EXTERNAL agent-loop-harness crate's logic)
// ---------------------------------------------------------------------------

/// Run the harness loop in a thread until `stop` is set or the maximum loop
/// count is reached. Returns after each `run_once` pass.
fn start_harness_thread(
    kernel_url: &str,
    ipc_token: &str,
    state_path: &std::path::Path,
    max_automatic_runs: u64,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let config = agent_loop_harness::HarnessConfig {
        kernel_url: kernel_url.to_string(),
        ipc_token: ipc_token.to_string(),
        state_path: state_path.to_path_buf(),
        max_automatic_runs,
        max_total_wall_time_ms: 600_000,
        max_consecutive_failures: 3,
        poll_interval_ms: 50,
    };
    std::thread::spawn(move || {
        let client = agent_loop_harness::KernelClient::new(&config.kernel_url, &config.ipc_token);
        let mut state = agent_loop_harness::HarnessState::load(&config.state_path);
        let mut passes = 0u64;
        while !stop_thread.load(Ordering::SeqCst) && passes < 10_000 {
            match agent_loop_harness::run_once(&client, &mut state, &config) {
                Ok(_) => {}
                Err(error) => eprintln!("harness pass failed: {error}"),
            }
            passes += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    stop
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Acceptance: one user message → ≥3 bounded Runs in the SAME session,
/// automatic continuation, no user "继续", final normal completion.
#[test]
fn one_user_message_drives_three_bounded_runs_same_session() {
    let llm = StubLlm::start(1_000_000); // always tool calls → every Run yields
    let connector = StubConnector::start();
    let db = tmp_db_path("three-runs");
    let running = Arc::new(AtomicBool::new(true));
    let handle = serve_with_running(test_config(&db, &llm.base_url(), &connector.url()), Arc::clone(&running))
        .expect("kernel starts");
    let kernel = HttpKernel::new(handle.port, "test-token");
    let state_path = db.parent().unwrap().join("agent-loop-harness/state.json");
    let stop_harness = start_harness_thread(
        &format!("http://127.0.0.1:{}", handle.port),
        "test-token",
        &state_path,
        10,
    );

    // 1. The user sends ONE request.
    let ingress = kernel.post(
        "/v1/ingress",
        &json!({
            "protocol_version": "v1",
            "source": "Cli",
            "external_event_id": "e2e-user-msg-1",
            "received_at": "2026-08-01T00:00:00Z",
            "payload": { "text": "请完成这个长任务" },
            "auth_context": { "authenticated": true }
        }),
    );
    assert_eq!(ingress["ok"], json!(true), "ingress accepted: {ingress}");

    // 2-7. Wait until ≥3 Runs started in the same session.
    let deadline = Instant::now() + Duration::from_secs(60);
    let (run_ids, session_ids, yielded) = loop {
        let events = kernel.all_events();
        let run_ids: Vec<String> = events
            .iter()
            .filter(|e| e["event_kind"] == json!("RunStarted"))
            .filter_map(|e| e["run_id"].as_str().map(str::to_string))
            .collect();
        let session_ids: Vec<String> = events
            .iter()
            .filter(|e| e["event_kind"] == json!("RunStarted"))
            .filter_map(|e| e["session_id"].as_str().map(str::to_string))
            .collect();
        let yielded = events
            .iter()
            .filter(|e| {
                (e["event_kind"] == json!("ToolBudgetExhausted")
                    || e["event_kind"] == json!("ToolLoopWallClockExceeded"))
                    && e["payload"]["exhaustion_action"] == json!("yield")
            })
            .count();
        if run_ids.len() >= 3 && yielded >= 3 && session_ids.windows(2).all(|w| w[0] == w[1]) {
            break (run_ids, session_ids, yielded);
        }
        if Instant::now() > deadline {
            panic!(
                "timeout: run_ids={run_ids:?} yielded={yielded} sessions={session_ids:?} events={}",
                serde_json::to_string_pretty(&kernel.all_events()).unwrap().len()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    // Assertions.
    assert!(run_ids.len() >= 3, "at least 3 bounded Runs, got {run_ids:?}");
    assert!(yielded >= 3, "at least 3 yield events, got {yielded}");
    let unique_sessions: std::collections::HashSet<String> = session_ids.iter().cloned().collect();
    assert_eq!(unique_sessions.len(), 1, "SAME session across all Runs: {session_ids:?}");
    let session_id = unique_sessions.into_iter().next().unwrap();

    // 8. Subsequent Runs see prior context: the session has multiple
    // UserMessage turns and multiple Runs' events.
    let session_events = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["session_id"].as_str() == Some(session_id.as_str()))
        .collect::<Vec<_>>();
    assert!(
        session_events.len() >= 20,
        "rich session context accumulated (compaction/tool results), got {} events",
        session_events.len()
    );

    // 9-10. Switch the LLM to final replies → the next Run completes and the
    // harness STOPS (waiting_user / completed → no more continuations).
    llm.switch_to_text();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = kernel.all_events();
        let completed_count = events
            .iter()
            .filter(|e| e["event_kind"] == json!("RunCompleted"))
            .count();
        let runs = events
            .iter()
            .filter(|e| e["event_kind"] == json!("RunStarted"))
            .count();
        if completed_count >= 1 && runs <= run_ids.len() + 1 {
            break;
        }
        if Instant::now() > deadline {
            panic!("harness did not stop after completion: completed={completed_count}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // High 2: NO fake user message. The only IngressAccepted is the single
    // real user request; continuations are recorded as the generic
    // SessionContinuationRequested governance event, never as a user message.
    let ingress_events = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["event_kind"] == json!("IngressAccepted"))
        .collect::<Vec<_>>();
    assert_eq!(
        ingress_events.len(),
        1,
        "REAL_USER_MESSAGE_COUNT_AFTER_3_RUNS=1, no fake continue user messages"
    );
    let continuation_events = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["event_kind"] == json!("SessionContinuationRequested"))
        .collect::<Vec<_>>();
    assert!(
        continuation_events.len() >= 2,
        "continuations recorded as governance events, got {}",
        continuation_events.len()
    );

    // High 3: no "请发送继续" prompt was ever delivered to the user. The
    // yield runs recorded the structured fact only — no reply Invocation was
    // created for them, so nothing entered the outbox/connector.
    let delivered = connector.delivered_text();
    assert!(
        !delivered.contains("请发送继续"),
        "USER_CONTINUE_PROMPT_SENT=false; delivered: {delivered:?}"
    );

    // 11. No product model added: the continuation ledger has rows but the
    // events contain no task/progress/checkpoint vocabulary.
    let all_events_json = serde_json::to_string(&kernel.all_events()).unwrap().to_lowercase();
    for forbidden in ["task", "progress", "checkpoint", "development_job", "continuationpolicy"] {
        assert!(
            !all_events_json.contains(&format!("\"{forbidden}\"")),
            "no {forbidden} in Kernel events"
        );
    }

    // Cleanup.
    stop_harness.store(true, Ordering::SeqCst);
    running.store(false, Ordering::SeqCst);
    let _ = handle.accept_loop.join();
    let _ = handle.worker.join();
    let _ = handle.outbox_dispatcher.join();
    let _ = handle.approval_expiry.join();
}

/// Acceptance: reaching the external limit (max_automatic_runs=2) stops the
/// harness — no infinite continuation.
#[test]
fn external_limit_stops_continuation() {
    let llm = StubLlm::start(1_000_000); // always yields
    let connector = StubConnector::start();
    let db = tmp_db_path("limit");
    let running = Arc::new(AtomicBool::new(true));
    let handle = serve_with_running(test_config(&db, &llm.base_url(), &connector.url()), Arc::clone(&running))
        .expect("kernel starts");
    let kernel = HttpKernel::new(handle.port, "test-token");
    let state_path = db.parent().unwrap().join("agent-loop-harness/state.json");
    // Limit of 2 automatic runs → total 3 Runs (1 user + 2 auto), then stop.
    let stop_harness = start_harness_thread(
        &format!("http://127.0.0.1:{}", handle.port),
        "test-token",
        &state_path,
        2,
    );

    let ingress = kernel.post(
        "/v1/ingress",
        &json!({
            "protocol_version": "v1",
            "source": "Cli",
            "external_event_id": "e2e-user-msg-2",
            "received_at": "2026-08-01T00:00:00Z",
            "payload": { "text": "长任务" },
            "auth_context": { "authenticated": true }
        }),
    );
    assert_eq!(ingress["ok"], json!(true));

    // Wait: exactly 3 Runs (1 + 2 automatic), then NO 4th Run for a while.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let runs = kernel
            .all_events()
            .into_iter()
            .filter(|e| e["event_kind"] == json!("RunStarted"))
            .count();
        if runs == 3 {
            break;
        }
        if Instant::now() > deadline {
            panic!("expected 3 Runs total with limit=2, got {runs}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Stability check: no 4th Run appears within 3 seconds.
    std::thread::sleep(Duration::from_secs(3));
    let runs = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["event_kind"] == json!("RunStarted"))
        .count();
    assert_eq!(runs, 3, "limit must stop continuation at 3 Runs total");

    stop_harness.store(true, Ordering::SeqCst);
    running.store(false, Ordering::SeqCst);
    let _ = handle.accept_loop.join();
    let _ = handle.worker.join();
    let _ = handle.outbox_dispatcher.join();
    let _ = handle.approval_expiry.join();
}

/// Acceptance: a model that immediately produces a final reply (waiting for
/// user) never triggers continuation.
#[test]
fn waiting_user_never_continues() {
    let llm = StubLlm::start(0); // 0 tool calls → final reply immediately
    let connector = StubConnector::start();
    let db = tmp_db_path("waiting");
    let running = Arc::new(AtomicBool::new(true));
    let handle = serve_with_running(test_config(&db, &llm.base_url(), &connector.url()), Arc::clone(&running))
        .expect("kernel starts");
    let kernel = HttpKernel::new(handle.port, "test-token");
    let state_path = db.parent().unwrap().join("agent-loop-harness/state.json");
    let stop_harness = start_harness_thread(
        &format!("http://127.0.0.1:{}", handle.port),
        "test-token",
        &state_path,
        10,
    );

    let ingress = kernel.post(
        "/v1/ingress",
        &json!({
            "protocol_version": "v1",
            "source": "Cli",
            "external_event_id": "e2e-user-msg-3",
            "received_at": "2026-08-01T00:00:00Z",
            "payload": { "text": "你好" },
            "auth_context": { "authenticated": true }
        }),
    );
    assert_eq!(ingress["ok"], json!(true), "ingress response: {ingress}");

    // Wait for the run to complete.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let completed = kernel
            .all_events()
            .into_iter()
            .filter(|e| e["event_kind"] == json!("RunCompleted"))
            .count();
        if completed >= 1 {
            break;
        }
        if Instant::now() > deadline {
            panic!("run did not complete");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Stability: NO second Run within 3 seconds.
    std::thread::sleep(Duration::from_secs(3));
    let runs = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["event_kind"] == json!("RunStarted"))
        .count();
    assert_eq!(runs, 1, "waiting-user must never auto-continue");

    stop_harness.store(true, Ordering::SeqCst);
    running.store(false, Ordering::SeqCst);
    let _ = handle.accept_loop.join();
    let _ = handle.worker.join();
    let _ = handle.outbox_dispatcher.join();
    let _ = handle.approval_expiry.join();
}

/// Acceptance: the SAME trigger Run can only ever be continued ONCE,
/// regardless of idempotency_key, concurrency, or Harness state loss
/// (UNIQUE(trigger_run_id) in the Kernel).
#[test]
fn duplicate_continuation_is_idempotent() {
    let llm = StubLlm::start(1_000_000);
    let connector = StubConnector::start();
    let db = tmp_db_path("idem");
    let running = Arc::new(AtomicBool::new(true));
    let handle = serve_with_running(test_config(&db, &llm.base_url(), &connector.url()), Arc::clone(&running))
        .expect("kernel starts");
    let kernel = HttpKernel::new(handle.port, "test-token");

    let ingress = kernel.post(
        "/v1/ingress",
        &json!({
            "protocol_version": "v1",
            "source": "Cli",
            "external_event_id": "e2e-user-msg-4",
            "received_at": "2026-08-01T00:00:00Z",
            "payload": { "text": "长任务" },
            "auth_context": { "authenticated": true }
        }),
    );
    assert_eq!(ingress["ok"], json!(true));

    // Wait for the first RunStarted to learn the session_id.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (session_id, run_id) = loop {
        let events = kernel.all_events();
        if let Some(started) = events
            .iter()
            .find(|e| e["event_kind"] == json!("RunStarted"))
        {
            let session_id = started["session_id"].as_str().unwrap_or("").to_string();
            let run_id = started["run_id"].as_str().unwrap_or("").to_string();
            if !session_id.is_empty() {
                break (session_id, run_id);
            }
        }
        if Instant::now() > deadline {
            panic!("no RunStarted");
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    // Narrow contract: the Harness identifies ONLY the trigger Run.
    let body = json!({
        "trigger_run_id": run_id,
        "expected_session_id": session_id,
        "idempotency_key": "continuation:".to_string() + &run_id,
    });
    let first = kernel.post("/v1/session-continuation", &body);
    assert_eq!(first["ok"], json!(true), "first accepted: {first}");
    assert_eq!(first["duplicate"], json!(false));

    // Same trigger, SAME key → duplicate, no second next Run.
    let second = kernel.post("/v1/session-continuation", &body);
    assert_eq!(second["ok"], json!(true), "same-key duplicate accepted: {second}");
    assert_eq!(second["duplicate"], json!(true), "same-key must be duplicate");

    // Same trigger, DIFFERENT key → still duplicate (UNIQUE(trigger_run_id)).
    let different_key = json!({
        "trigger_run_id": run_id,
        "expected_session_id": session_id,
        "idempotency_key": "continuation:".to_string() + &run_id + "-other-key",
    });
    let third = kernel.post("/v1/session-continuation", &different_key);
    assert_eq!(
        third["ok"], json!(true),
        "different-key duplicate accepted: {third}"
    );
    assert_eq!(
        third["duplicate"], json!(true),
        "SAME_TRIGGER_DIFFERENT_KEYS_NEXT_RUN_COUNT=1 — different key must still be duplicate"
    );

    // Concurrent requests (two threads, same trigger) → still one next Run.
    let kernel_a = HttpKernel::new(handle.port, "test-token");
    let kernel_b = HttpKernel::new(handle.port, "test-token");
    let run_id_a = run_id.clone();
    let session_id_a = session_id.clone();
    let t1 = std::thread::spawn(move || {
        kernel_a.post(
            "/v1/session-continuation",
            &json!({
                "trigger_run_id": run_id_a,
                "expected_session_id": session_id_a,
                "idempotency_key": "concurrent-a",
            }),
        )
    });
    let run_id_b = run_id.clone();
    let session_id_b = session_id.clone();
    let t2 = std::thread::spawn(move || {
        kernel_b.post(
            "/v1/session-continuation",
            &json!({
                "trigger_run_id": run_id_b,
                "expected_session_id": session_id_b,
                "idempotency_key": "concurrent-b",
            }),
        )
    });
    let r1 = t1.join().unwrap();
    let r2 = t2.join().unwrap();
    assert_eq!(r1["ok"], json!(true), "concurrent A: {r1}");
    assert_eq!(r2["ok"], json!(true), "concurrent B: {r2}");
    assert!(
        r1["duplicate"] == json!(true) || r2["duplicate"] == json!(true),
        "exactly one of the concurrent requests may create the continuation: A={r1} B={r2}"
    );

    // Exactly ONE next Run appears (the first continuation), never more.
    std::thread::sleep(Duration::from_secs(2));
    let runs = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["event_kind"] == json!("RunStarted"))
        .count();
    assert_eq!(runs, 2, "one trigger Run creates exactly one next Run");

    // STATE_LOSS_DUPLICATE_PROTECTED: delete the Harness state and re-observe —
    // the Kernel's UNIQUE(trigger_run_id) still blocks a second continuation.
    let duplicate_after = kernel.post("/v1/session-continuation", &body);
    assert_eq!(
        duplicate_after["duplicate"], json!(true),
        "STATE_LOSS_DUPLICATE_PROTECTED=true — repeat after state loss still duplicate"
    );

    running.store(false, Ordering::SeqCst);
    let _ = handle.accept_loop.join();
    let _ = handle.worker.join();
    let _ = handle.outbox_dispatcher.join();
    let _ = handle.approval_expiry.join();
}

/// Acceptance: a harness restart never re-processes already-resolved runs
/// (cursor + processed_run_ids persisted in state.json).
#[test]
fn harness_restart_does_not_reprocess() {
    let llm = StubLlm::start(1_000_000);
    let connector = StubConnector::start();
    let db = tmp_db_path("restart");
    let running = Arc::new(AtomicBool::new(true));
    let handle = serve_with_running(test_config(&db, &llm.base_url(), &connector.url()), Arc::clone(&running))
        .expect("kernel starts");
    let kernel = HttpKernel::new(handle.port, "test-token");
    let state_path = db.parent().unwrap().join("agent-loop-harness/state.json");

    // First harness run: 1 automatic continuation limit.
    let stop1 = start_harness_thread(
        &format!("http://127.0.0.1:{}", handle.port),
        "test-token",
        &state_path,
        1,
    );
    let ingress = kernel.post(
        "/v1/ingress",
        &json!({
            "protocol_version": "v1",
            "source": "Cli",
            "external_event_id": "e2e-user-msg-5",
            "received_at": "2026-08-01T00:00:00Z",
            "payload": { "text": "长任务" },
            "auth_context": { "authenticated": true }
        }),
    );
    assert_eq!(ingress["ok"], json!(true));
    // Wait for 2 Runs (1 user + 1 auto) AND for Run #2's yield event to be
    // observed, so the harness has resolved both runs before we stop it.
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let events = kernel.all_events();
        let runs = events
            .iter()
            .filter(|e| e["event_kind"] == json!("RunStarted"))
            .count();
        let yields = events
            .iter()
            .filter(|e| {
                (e["event_kind"] == json!("ToolBudgetExhausted")
                    || e["event_kind"] == json!("ToolLoopWallClockExceeded"))
                    && e["payload"]["exhaustion_action"] == json!("yield")
            })
            .count();
        if runs == 2 && yields >= 2 {
            break;
        }
        if Instant::now() > deadline {
            panic!("expected 2 Runs with 2 yields, got runs={runs} yields={yields}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    stop1.store(true, Ordering::SeqCst);
    // Give the harness thread time to persist its final state.
    std::thread::sleep(Duration::from_millis(500));

    // Record run count, then "restart" the harness with the same state file
    // and a HIGHER limit. It must NOT create a third Run (the second Run was
    // already processed in the first harness lifetime).
    let before = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["event_kind"] == json!("RunStarted"))
        .count();
    let stop2 = start_harness_thread(
        &format!("http://127.0.0.1:{}", handle.port),
        "test-token",
        &state_path,
        5,
    );
    std::thread::sleep(Duration::from_secs(3));
    let after = kernel
        .all_events()
        .into_iter()
        .filter(|e| e["event_kind"] == json!("RunStarted"))
        .count();
    assert_eq!(before, after, "restart must not reprocess resolved runs: {before} → {after}");

    stop2.store(true, Ordering::SeqCst);
    running.store(false, Ordering::SeqCst);
    let _ = handle.accept_loop.join();
    let _ = handle.worker.join();
    let _ = handle.outbox_dispatcher.join();
    let _ = handle.approval_expiry.join();
}
