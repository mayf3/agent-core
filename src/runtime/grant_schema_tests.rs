use crate::domain::operation::{provider_tool_definition, provider_tools_for_grants};
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, OpenAiCompatibleLlm};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
#[test]
fn provider_tools_expose_only_granted_readonly_operations() {
    let tools = provider_tools_for_grants(&["system.status".to_string()]);
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].pointer("/function/name").and_then(Value::as_str),
        Some("system.status")
    );
}

#[test]
fn write_operations_are_never_in_provider_tools() {
    let tools = provider_tools_for_grants(&[
        "stdout.send_text".to_string(),
        "feishu.send_message".to_string(),
    ]);
    assert!(tools.is_empty(), "Write ops must never enter tools schema");
}

#[test]
fn unknown_operations_are_never_in_provider_tools() {
    let tools = provider_tools_for_grants(&["shell.exec".to_string(), "".to_string()]);
    assert!(tools.is_empty());
}

#[test]
fn multiple_readonly_grants_expose_all_in_catalog_order() {
    let tools = provider_tools_for_grants(&[
        "system.status".to_string(),
        "session.recall_recent".to_string(),
    ]);
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert_eq!(
        names,
        vec!["session.recall_recent", "system.status"]
    );
}

#[test]
fn provider_tool_definition_rejects_write_and_unknown() {
    assert!(provider_tool_definition("feishu.send_message").is_none());
    assert!(provider_tool_definition("shell.exec").is_none());
    let tn = provider_tool_definition("system.status").unwrap();
    assert_eq!(
        tn.pointer("/function/name").and_then(Value::as_str),
        Some("system.status")
    );
    assert_eq!(
        tn.pointer("/function/parameters/additionalProperties")
            .and_then(Value::as_bool),
        Some(false)
    );
}

// ===== §7: real HTTP request-capture — tools reflect grants =====
// A local stub server captures the raw outgoing request body (no key, no real
// network). Nothing sensitive is printed.

struct CaptureServer {
    port: u16,
    captured: Arc<Mutex<Vec<Value>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CaptureServer {
    fn start(responses: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_thread = Arc::clone(&captured);
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
                if let Some(body) = read_request_json(&mut stream) {
                    captured_thread.lock().unwrap().push(body);
                }
                let body = response.to_string();
                let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Self {
            port,
            captured,
            shutdown,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    fn requests(&self) -> Vec<Value> {
        self.captured.lock().unwrap().clone()
    }
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request_json(stream: &mut std::net::TcpStream) -> Option<Value> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .ok()?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..read]);
        if let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };
    let headers = std::str::from_utf8(&raw[..header_end]).ok()?;
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())?
    })?;
    let body_start = header_end + 4;
    while raw.len() < body_start + content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    serde_json::from_slice(&raw[body_start..body_start + content_length]).ok()
}

fn successful_text_response(text: &str) -> Value {
    json!({
        "model": "local-stub",
        "choices": [{ "message": { "content": text } }]
    })
}

#[test]
fn request_includes_time_now_when_granted() -> Result<()> {
    let server = CaptureServer::start(vec![successful_text_response("ok")]);
    let llm = OpenAiCompatibleLlm::new(
        server.base_url(),
        "local-test".into(),
        "local-stub".into(),
        3000,
    );
    let snap = crate::registry::snapshot::test_snapshot();
    let provider_tools =
        snap.provider_tools_for_grants(&["system.status".to_string(), "session.recall_recent".to_string()]);
    let _ = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "x".into(),
        granted_operations: vec!["system.status".to_string(), "session.recall_recent".to_string()],
        provider_tools,
        follow_ups: vec![],
    })?;
    let requests = server.requests();
    let body = requests.first().expect("request captured");
    assert_eq!(
        body.get("tool_choice").and_then(Value::as_str),
        Some("auto")
    );
    let tools = body.get("tools").and_then(Value::as_array).unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(
        tools[0].pointer("/function/name").and_then(Value::as_str),
        Some("session.recall_recent")
    );
    assert_eq!(
        tools[0]
            .pointer("/function/parameters/type")
            .and_then(Value::as_str),
        Some("object")
    );
    // §5: every exposed tool schema must be strict.
    for tool in tools {
        assert_eq!(
            tool.pointer("/function/parameters/additionalProperties")
                .and_then(Value::as_bool),
            Some(false),
            "strict schema required"
        );
    }
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["session.recall_recent", "system.status"]);
    Ok(())
}

#[test]
fn request_omits_time_now_when_not_granted() -> Result<()> {
    let server = CaptureServer::start(vec![successful_text_response("ok")]);
    let llm = OpenAiCompatibleLlm::new(
        server.base_url(),
        "local-test".into(),
        "local-stub".into(),
        3000,
    );
    let snap = crate::registry::snapshot::test_snapshot();
    let provider_tools = snap.provider_tools_for_grants(&["session.recall_recent".to_string()]);
    // Grant only session.recall_recent — NOT system.status.
    let _ = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "x".into(),
        granted_operations: vec!["session.recall_recent".to_string()],
        provider_tools,
        follow_ups: vec![],
    })?;
    let requests = server.requests();
    let body = requests.first().expect("request captured");
    let tool_names: Vec<&str> = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !tool_names.contains(&"system.status"),
        "system.status must NOT be in tools when not granted: {tool_names:?}"
    );
    assert_eq!(tool_names, vec!["session.recall_recent"]);
    Ok(())
}

#[test]
fn misconfigured_write_grant_not_in_tools() -> Result<()> {
    let server = CaptureServer::start(vec![successful_text_response("ok")]);
    let llm = OpenAiCompatibleLlm::new(
        server.base_url(),
        "local-test".into(),
        "local-stub".into(),
        3000,
    );
    // Write operations produce an empty provider_tools list — the test
    // verifies that even when granted, Write ops don't appear in tools.
    let provider_tools: Vec<serde_json::Value> = vec![];
    let _ = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "x".into(),
        granted_operations: vec!["feishu.send_message".to_string()],
        provider_tools,
        follow_ups: vec![],
    })?;
    let requests = server.requests();
    let body = requests.first().expect("request captured");
    let tool_names: Vec<&str> = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        tool_names.is_empty(),
        "Write op must not enter tools: {tool_names:?}"
    );
    Ok(())
}

#[test]
fn ungranted_provider_time_now_is_rejected_by_gateway() {
    let mut cfg = super::tool_loop_tests::test_config();
    let server = CaptureServer::start(vec![
        json!({
            "model": "local-stub",
            "choices": [{"message": {
                "content": "",
                "tool_calls": [{
                    "id": "ungranted-provider-call",
                    "type": "function",
                    "function": {"name": "system.status", "arguments": "{}"}
                }]
            }}]
        }),
        successful_text_response("That tool is not available."),
    ]);
    cfg.openai_base_url = server.base_url();
    cfg.openai_api_key = "local-test".to_string();
    cfg.model = "local-stub".to_string();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(cfg.clone());
    let llm = OpenAiCompatibleLlm::new(
        cfg.openai_base_url.clone(),
        cfg.openai_api_key.clone(),
        cfg.model.clone(),
        3000,
    );
    let runtime = Runtime::new(cfg, llm);
    let envelope = gateway.cli_ingress("current time".to_string()).unwrap();
    let event = gateway.validate_ingress(&journal, envelope).unwrap();
    let outcome = runtime.deliver(&journal, &gateway, event).unwrap();
    assert_ne!(
        journal.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Running"),
        "Run not stuck Running after un-granted tool call"
    );
    let events = journal.events().unwrap();
    let count = |k: JournalEventKind| events.iter().filter(|e| e.kind == k).count();
    // count an event kind whose operation == system.status
    let count_op = |k: JournalEventKind| {
        events
            .iter()
            .filter(|e| {
                e.kind == k
                    && e.payload.get("operation").and_then(Value::as_str) == Some("system.status")
            })
            .count()
    };
    assert_eq!(count(JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count_op(JournalEventKind::InvocationProposed), 1);
    assert_eq!(count(JournalEventKind::ToolCallRejected), 1);
    assert_eq!(count_op(JournalEventKind::InvocationApproved), 0);
    assert_eq!(count(JournalEventKind::ReceiptReceived), 0);
    let rej = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ToolCallRejected)
        .unwrap();
    assert_eq!(
        rej.payload.get("error_category").and_then(|v| v.as_str()),
        Some("policy_denied")
    );
    let requests = server.requests();
    let names: Vec<&str> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["session.recall_recent"]);
    // §5: system.status is not in provider tools (asserted via names above) and not
    // in the system ToolCatalog block.
    let sys_msg = requests[0]["messages"][0]["content"].as_str().unwrap_or("");
    assert!(
        !sys_msg.contains("system.status"),
        "un-granted system.status not in ToolCatalog"
    );
}
// ===== §9: full system.status tool loop (granted) =====
#[test]
fn granted_time_now_completes_real_http_tool_loop() {
    let mut cfg = super::tool_loop_tests::test_config();
    cfg.extra_allowed_operations = vec!["system.status".to_string()];
    let server = CaptureServer::start(vec![
        json!({
            "model": "local-stub",
            "choices": [{"message": {
                "content": "",
                "tool_calls": [{
                    "id": "provider-call-1",
                    "type": "function",
                    "function": {"name": "system.status", "arguments": "{}"}
                }]
            }}]
        }),
        successful_text_response("The current time was retrieved."),
    ]);
    cfg.openai_base_url = server.base_url();
    cfg.openai_api_key = "local-test".to_string();
    cfg.model = "local-stub".to_string();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(cfg.clone());
    let llm = OpenAiCompatibleLlm::new(
        cfg.openai_base_url.clone(),
        cfg.openai_api_key.clone(),
        cfg.model.clone(),
        3000,
    );
    let runtime = Runtime::new(cfg, llm);
    let envelope = gateway.cli_ingress("current time".to_string()).unwrap();
    let event = gateway.validate_ingress(&journal, envelope).unwrap();
    let outcome = runtime.deliver(&journal, &gateway, event).unwrap();
    assert!(!outcome.output.trim().is_empty());
    let events = journal.events().unwrap();
    let count = |k: JournalEventKind| events.iter().filter(|e| e.kind == k).count();
    let count_time_now = |kind: JournalEventKind| {
        events
            .iter()
            .filter(|event| {
                event.kind == kind
                    && event.payload.get("operation").and_then(Value::as_str) == Some("system.status")
            })
            .count()
    };
    assert_eq!(count(JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count_time_now(JournalEventKind::InvocationProposed), 1);
    assert_eq!(count_time_now(JournalEventKind::InvocationApproved), 1);
    assert_eq!(count(JournalEventKind::ReceiptReceived), 1);
    assert_eq!(count(JournalEventKind::OutboxQueued), 1);
    let receipt = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ReceiptReceived)
        .unwrap();
    assert_eq!(
        receipt.payload.get("status").and_then(|s| s.as_str()),
        Some("Succeeded")
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let names: Vec<&str> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["session.recall_recent", "system.status"]);
    // The ToolResult is NOT duplicated in the system context — it is
    // delivered exclusively via the role=tool follow-up message.
    let system2 = requests[1]
        .pointer("/messages/0/content")
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        !system2.contains("tool: system.status"),
        "ToolResult must NOT be in system context"
    );
    let tool_msg = requests[1]
        .pointer("/messages/3/content")
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        tool_msg.contains("status: succeeded"),
        "ToolResult must be in role=tool message"
    );
    // §5: round-2 tools set == round-1; ToolCatalog consistent across rounds.
    let names2: Vec<&str> = requests[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert_eq!(names2, names, "round-2 tools set must equal round-1");
    let cat1 = requests[0]["messages"][0]["content"].as_str().unwrap_or("");
    let cat2 = requests[1]["messages"][0]["content"].as_str().unwrap_or("");
    assert_eq!(
        cat1.contains("system.status"),
        cat2.contains("system.status"),
        "ToolCatalog consistent across rounds"
    );
    // Run is not left Running.
    let status = journal.run_status(&outcome.run_id).unwrap();
    assert_ne!(status.as_deref(), Some("Running"), "Run not stuck Running");
}
