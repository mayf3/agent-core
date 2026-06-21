use agent_core_kernel::llm::{LlmClient, LlmInput, OpenAiCompatibleLlm};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
#[test]
fn fallback_endpoint_is_used_after_primary_http_error() -> Result<()> {
    let primary = serve_once(400, json!({ "error": { "message": "bad model" } }))?;
    let fallback = serve_once(
        200,
        json!({
            "model": "deepseek-v4-flash",
            "choices": [{ "message": { "content": "fallback ok" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        }),
    )?;
    let llm = OpenAiCompatibleLlm::new(primary, "primary-key".into(), "bad-primary".into(), 2_000)
        .with_fallback(fallback, "fallback-key".into(), "deepseek-v4-flash".into());
    let output = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "hello".into(),
        granted_operations: vec![],
    })?;
    assert_eq!(output.model, "deepseek-v4-flash");
    assert_eq!(output.content, "fallback ok");
    assert_eq!(
        output
            .journal_payload
            .pointer("/fallback/used")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output
            .journal_payload
            .pointer("/fallback/primary_error_category")
            .and_then(Value::as_str),
        Some("model_http_400")
    );
    Ok(())
}
fn serve_once(status: u16, body: Value) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut _buf = [0u8; 8192];
            let _ = stream.read(&mut _buf);
            let body_str = body.to_string();
            let st = if status == 200 { "OK" } else { "Error" };
            let resp = format!("HTTP/1.1 {status} {st}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body_str.len(), body_str);
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    Ok(format!("http://{addr}/v1"))
}
mod common;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmOutput, ToolCall, ToolCallResult};
use agent_core_kernel::runtime::Runtime;
struct RecallThenAnswerLlm {
    round: Arc<Mutex<usize>>,
    saw_tool_result_block: Arc<Mutex<bool>>,
}

impl LlmClient for RecallThenAnswerLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        let mut round = self.round.lock().unwrap();
        let current = *round;
        *round += 1;
        if current >= 1 {
            *self.saw_tool_result_block.lock().unwrap() = input
                .blocks
                .iter()
                .any(|b| matches!(b.kind, ContextBlockKind::ToolResult));
        }
        if current == 0 {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "recall-loop".into(),
                content: "let me recall".into(),
                journal_payload: json!({ "round": current }),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: agent_core_kernel::llm::tool_call_id_hash("recall_round_0"),
                    operation: "session.recall_recent".into(),
                    arguments: json!({ "limit": 5 }),
                }),
            })
        } else {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "recall-loop".into(),
                content: "The PR5 risk was WaitingDispatch not closing the loop.".into(),
                journal_payload: json!({ "round": current }),
                tool_call: ToolCallResult::Absent,
            })
        }
    }
}

#[test]
fn recall_loop_uses_second_round_reply() -> Result<()> {
    let config = common::test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let llm = RecallThenAnswerLlm {
        round: Arc::new(Mutex::new(0)),
        saw_tool_result_block: Arc::new(Mutex::new(false)),
    };
    let runtime = Runtime::new(config, llm);
    let envelope = gateway.cli_ingress("what was the PR5 risk".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert!(
        outcome.output.contains("WaitingDispatch"),
        "reply must be from second LLM round: {}",
        outcome.output
    );
    assert!(
        !outcome.output.contains("let me recall"),
        "first-round placeholder must not be the final reply"
    );
    Ok(())
}

#[test]
fn recall_loop_appends_tool_result_block_before_second_round() -> Result<()> {
    let config = common::test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let saw = Arc::new(Mutex::new(false));
    let llm = RecallThenAnswerLlm {
        round: Arc::new(Mutex::new(0)),
        saw_tool_result_block: Arc::clone(&saw),
    };
    let runtime = Runtime::new(config, llm);
    let envelope = gateway.cli_ingress("what was the PR5 risk".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let _ = runtime.deliver(&journal, &gateway, event)?;
    assert!(
        *saw.lock().unwrap(),
        "second LLM round must see ToolResult block"
    );
    Ok(())
}

struct AlwaysRecallLlm {
    calls: Mutex<usize>,
}

impl LlmClient for AlwaysRecallLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        *self.calls.lock().unwrap() += 1;
        Ok(LlmOutput {
            provider: "test".into(),
            model: "always-recall".into(),
            content: format!("round {}", *self.calls.lock().unwrap() - 1),
            journal_payload: json!({}),
            tool_call: ToolCallResult::Valid(ToolCall {
                id: agent_core_kernel::llm::tool_call_id_hash("always_recall"),
                operation: "session.recall_recent".into(),
                arguments: json!({ "limit": 5 }),
            }),
        })
    }
}

#[test]
fn recall_loop_is_bounded_by_max_tool_rounds() -> Result<()> {
    let config = common::test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(
        config,
        AlwaysRecallLlm {
            calls: Mutex::new(0),
        },
    );
    let envelope = gateway.cli_ingress("keep recalling".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    let events = journal.events()?;
    let re: Vec<_> = events
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&outcome.run_id))
        .collect();
    let llm = re
        .iter()
        .filter(|e| e.kind == JournalEventKind::LlmCompleted)
        .count();
    let rec = re
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .count();
    let oq = re
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .count();
    let tool_ops: Vec<&str> = re
        .iter()
        .filter_map(|e| {
            if e.kind == JournalEventKind::InvocationProposed {
                e.payload.get("operation").and_then(|v| v.as_str())
            } else {
                None
            }
        })
        .collect();
    let tool_proposals = tool_ops
        .iter()
        .filter(|op| **op != "stdout.send_text")
        .count();
    let reply_proposals = tool_ops
        .iter()
        .filter(|op| **op == "stdout.send_text")
        .count();
    assert_eq!(
        llm, 3,
        "LlmCompleted (round 0 tool, round 1 tool, round 2 reply)"
    );
    assert_eq!(rec, 2, "ReceiptReceived (2 tool executions, 0 for reply)");
    assert_eq!(
        tool_proposals, 2,
        "tool InvocationProposed (session.recall_recent)"
    );
    assert_eq!(
        reply_proposals, 1,
        "reply InvocationProposed (stdout.send_text)"
    );
    assert_eq!(oq, 1, "OutboxQueued (only the reply, not tools)");
    assert!(!outcome.output.is_empty(), "final reply");
    Ok(())
}

struct ProposeUnknownToolLlm {
    round: Arc<Mutex<usize>>,
    saw_error_block: Arc<Mutex<bool>>,
}

impl LlmClient for ProposeUnknownToolLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        let mut round = self.round.lock().unwrap();
        let current = *round;
        *round += 1;
        if current >= 1 {
            *self.saw_error_block.lock().unwrap() = input.blocks.iter().any(|b| {
                matches!(b.kind, ContextBlockKind::ToolResult)
                    && (b.content.contains("rejected") || b.content.contains("error"))
            });
            return Ok(LlmOutput {
                provider: "test".into(),
                model: "unknown-tool".into(),
                content: "sorry, that tool is unavailable".into(),
                journal_payload: json!({ "round": current }),
                tool_call: ToolCallResult::Absent,
            });
        }
        Ok(LlmOutput {
            provider: "test".into(),
            model: "unknown-tool".into(),
            content: "trying a tool".into(),
            journal_payload: json!({ "round": current }),
            tool_call: ToolCallResult::Valid(ToolCall {
                id: agent_core_kernel::llm::tool_call_id_hash("unknown_tool"),
                operation: "shell.exec".into(),
                arguments: json!({}),
            }),
        })
    }
}

#[test]
fn recall_loop_does_not_crash_on_tool_failure() -> Result<()> {
    let config = common::test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let saw_error = Arc::new(Mutex::new(false));
    let llm = ProposeUnknownToolLlm {
        round: Arc::new(Mutex::new(0)),
        saw_error_block: Arc::clone(&saw_error),
    };
    let runtime = Runtime::new(config, llm);
    let envelope = gateway.cli_ingress("run something dangerous".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert!(
        outcome.output.contains("unavailable"),
        "model recovers: {}",
        outcome.output
    );
    assert!(*saw_error.lock().unwrap(), "ToolResult block fed back");
    Ok(())
}

#[test]
fn recall_loop_is_noop_when_no_tool_call() -> Result<()> {
    struct PlainLlm;
    impl LlmClient for PlainLlm {
        fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "plain".into(),
                content: "hello back".into(),
                journal_payload: json!({}),
                tool_call: ToolCallResult::Absent,
            })
        }
    }
    let config = common::test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, PlainLlm);
    let envelope = gateway.cli_ingress("hi".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert_eq!(outcome.output, "hello back");
    let events = journal.events()?;
    let re: Vec<_> = events
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&outcome.run_id))
        .collect();
    assert_eq!(
        re.iter()
            .filter(|e| e.kind == JournalEventKind::LlmCompleted)
            .count(),
        1,
        "LlmCompleted"
    );
    assert_eq!(
        re.iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .count(),
        0,
        "ReceiptReceived"
    );
    assert_eq!(
        re.iter()
            .filter(|e| e.kind == JournalEventKind::OutboxQueued)
            .count(),
        1,
        "OutboxQueued"
    );
    Ok(())
}

struct StubServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_thread = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            let mut round = 0usize;
            loop {
                let (mut stream, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                if shutdown_thread.load(Ordering::Relaxed) {
                    break;
                }
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(2000)));
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let body_str = match round {
                    0 | 1 => {
                        round += 1;
                        r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call_stub_1","type":"function","function":{"name":"time.now","arguments":"{}"}}]}}],"model":"stub"}"#
                    }
                    _ => {
                        round += 1;
                        r#"{"choices":[{"message":{"content":"The current time was retrieved successfully."}}],"model":"stub"}"#
                    }
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_str.len(),
                    body_str
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                if round >= 3 {
                    break;
                }
            }
        });
        Self {
            port,
            shutdown,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Ok(socket) = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], self.port)),
            std::time::Duration::from_millis(500),
        ) {
            drop(socket);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[test]
fn stub_http_provider_completes_tool_loop() -> Result<()> {
    let server = StubServer::start();
    let mut config = common::test_config();
    config.openai_base_url = format!("{}/v1", server.base_url());
    config.openai_api_key = "stub-key".to_string();
    config.model = "stub".to_string();
    config.extra_allowed_operations = vec!["time.now".to_string(), "system.status".to_string()];

    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let llm = OpenAiCompatibleLlm::new(
        config.openai_base_url.clone(),
        config.openai_api_key.clone(),
        config.model.clone(),
        5000,
    );
    let runtime = Runtime::new(config, llm);
    let envelope = gateway.cli_ingress("what time is it".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;

    assert!(
        !outcome.output.is_empty(),
        "final reply must be non-empty, got: '{}'",
        outcome.output
    );
    let events = journal.events()?;
    assert!(
        events
            .iter()
            .any(|e| e.kind == JournalEventKind::ReceiptReceived),
        "tool execution receipt must be journaled"
    );
    let llm_rounds = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::LlmCompleted)
        .count();
    assert!(
        llm_rounds == 3,
        "exactly 3 LLM rounds expected, got {}",
        llm_rounds
    );

    let journal_json = serde_json::to_string(&events)?;
    assert!(
        !journal_json.contains("call_stub_1"),
        "raw provider ID leaked"
    );
    let keys: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::InvocationProposed
                && e.payload.get("source").and_then(|s| s.as_str()) == Some("model_tool_call")
        })
        .map(|e| {
            e.payload
                .get("idempotency_key")
                .and_then(|k| k.as_str())
                .unwrap()
        })
        .collect();
    let provider_id = agent_core_kernel::llm::tool_call_id_hash("call_stub_1");
    assert_eq!(
        keys,
        vec![
            format!("tool:{}:0:0:{provider_id}", outcome.run_id.0),
            format!("tool:{}:1:1:{provider_id}", outcome.run_id.0),
        ]
    );

    Ok(())
}
