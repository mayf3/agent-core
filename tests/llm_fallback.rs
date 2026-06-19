use agent_core_kernel::llm::{LlmClient, LlmInput, OpenAiCompatibleLlm};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
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
            let _ = read_http_request(&mut stream);
            let body = body.to_string();
            let st = if status == 200 { "OK" } else { "Error" };
            let resp = format!("HTTP/1.1 {status} {st}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    Ok(format!("http://{addr}/v1"))
}

fn read_http_request(stream: &mut TcpStream) -> Result<()> {
    let mut buf = [0_u8; 2048];
    let _ = stream.read(&mut buf)?;
    Ok(())
}

mod common;

use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmOutput, ToolCall};
use agent_core_kernel::runtime::Runtime;
use std::sync::{Arc, Mutex};

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
                tool_call: Some(ToolCall {
                    id: "recall_round_0".into(),
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
                tool_call: None,
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
            tool_call: Some(ToolCall {
                id: "always_recall".into(),
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
    let ip = re
        .iter()
        .filter(|e| e.kind == JournalEventKind::InvocationProposed)
        .count();
    assert_eq!(llm, 3, "LlmCompleted");
    assert_eq!(rec, 2, "ReceiptReceived");
    assert_eq!(ip, 3, "InvocationProposed");
    assert_eq!(oq, 1, "OutboxQueued");
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
                matches!(b.kind, ContextBlockKind::ToolResult) && b.content.contains("error")
            });
            return Ok(LlmOutput {
                provider: "test".into(),
                model: "unknown-tool".into(),
                content: "sorry, that tool is unavailable".into(),
                journal_payload: json!({ "round": current }),
                tool_call: None,
            });
        }
        Ok(LlmOutput {
            provider: "test".into(),
            model: "unknown-tool".into(),
            content: "trying a tool".into(),
            journal_payload: json!({ "round": current }),
            tool_call: Some(ToolCall {
                id: "unknown_tool".into(),
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
                tool_call: None,
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

fn read_http_request_body(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = stream.read(&mut b)?;
        if n == 0 {
            anyhow::bail!("eof");
        }
        buf.push(b[0]);
        if buf.len() >= 4 && buf[buf.len() - 4..] == [b'\r', b'\n', b'\r', b'\n'] {
            break;
        }
    }
    let header = String::from_utf8_lossy(&buf);
    let content_len: usize = header
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        let mut offset = 0;
        while offset < content_len {
            let n = stream.read(&mut body[offset..])?;
            if n == 0 {
                anyhow::bail!("eof");
            }
            offset += n;
        }
    }
    Ok(String::from_utf8(body)?)
}

struct StubServer {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let r = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            for round in 0..2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let body = match read_http_request_body(&mut stream) {
                    Ok(b) => b,
                    Err(_) => return,
                };
                if let Ok(body_val) = serde_json::from_str::<Value>(&body) {
                    r.lock().unwrap().push(body_val);
                }
                let response_body = if round == 0 {
                    r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call_stub_1","type":"function","function":{"name":"time.now","arguments":"{}"}}]}}],"model":"stub"}"#
                } else {
                    r#"{"choices":[{"message":{"content":"The current time was retrieved."}}],"model":"stub"}"#
                };
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response_body.len(), response_body);
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        Self {
            port,
            requests,
            handle: Some(handle),
        }
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[test]
fn stub_http_provider_completes_tool_loop() -> Result<()> {
    let server = StubServer::start();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut config = common::test_config();
    config.openai_base_url = server.url();
    config.openai_api_key = "stub-key".into();
    config.model = "stub".into();
    config.extra_allowed_operations = vec!["system.status".into(), "time.now".into()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let llm = OpenAiCompatibleLlm::new(
        config.openai_base_url.clone(),
        config.openai_api_key.clone(),
        config.model.clone(),
        5000,
    );
    let runtime = Runtime::new(config, llm);
    let envelope = gateway.cli_ingress("what time is it".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert_eq!(outcome.output, "The current time was retrieved.");

    let events = journal.events()?;
    let re: Vec<_> = events
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&outcome.run_id))
        .collect();
    assert_eq!(
        re.iter()
            .filter(|e| e.kind == JournalEventKind::LlmCompleted)
            .count(),
        2,
        "LlmCompleted"
    );
    assert_eq!(
        re.iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .count(),
        1,
        "ReceiptReceived"
    );

    let captured = server.requests.lock().unwrap();
    assert!(captured.len() >= 2, "HTTP requests");
    let req1 = &captured[0];
    assert_eq!(req1.get("model").and_then(Value::as_str), Some("stub"));
    let tools = req1.get("tools").and_then(Value::as_array).unwrap();
    assert_eq!(tools.len(), 3, "3 tool schemas");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"time.now"));
    assert!(names.contains(&"session.recall_recent"));
    assert!(names.contains(&"system.status"));
    assert!(!names.contains(&"feishu.send_message") && !names.contains(&"stdout.send_text"));
    assert_eq!(
        req1.get("tool_choice").and_then(Value::as_str),
        Some("auto"),
        "tool_choice"
    );

    let req2 = &captured[1];
    let sys = req2
        .pointer("/messages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .unwrap();
    let c = sys.get("content").and_then(Value::as_str).unwrap_or("");
    assert!(c.contains("ToolResult"), "ToolResult in sys msg");
    assert!(c.contains("time.now"), "tool ref in sys msg");
    Ok(())
}
