use agent_core_kernel::llm::{LlmClient, LlmInput, OpenAiCompatibleLlm};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
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
        provider_tools: vec![],
        follow_ups: vec![],
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
                provider_turn: None,
            })
        } else {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "recall-loop".into(),
                content: "The PR5 risk was WaitingDispatch not closing the loop.".into(),
                journal_payload: json!({ "round": current }),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
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
            provider_turn: None,
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
                provider_turn: None,
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

/// A stub HTTP server that serves a deterministic queue of responses.
/// Each incoming connection consumes one response from the queue in order.
/// The thread exits when the queue is empty. No timers, no round counters.
struct QueuedStub {
    port: u16,
    _handle: std::thread::JoinHandle<()>,
}

impl QueuedStub {
    fn new(responses: Vec<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut iter = responses.into_iter();
            while let Some(body) = iter.next() {
                let (mut stream, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(2000)));
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(2000)));
                // Read the full request so the TCP connection isn't RST-aborted
                // while the client is still reading our response (the original
                // flake cause). Read until the header boundary + Content-Length.
                let mut buf = Vec::with_capacity(8192);
                let mut tmp = [0u8; 4096];
                loop {
                    match stream.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let body_start = pos + 4;
                                let headers = std::str::from_utf8(&buf[..body_start]).unwrap_or("");
                                let clen = headers
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        k.eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse::<usize>().ok())
                                    })
                                    .flatten()
                                    .unwrap_or(0);
                                if clen == 0 || buf.len() >= body_start + clen {
                                    break;
                                }
                            }
                        }
                    }
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        Self {
            port,
            _handle: handle,
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

const TOOL_CALL_RESPONSE: &str = r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call_stub_1","type":"function","function":{"name":"time.now","arguments":"{}"}}]}}],"model":"stub"}"#;
const TEXT_RESPONSE: &str = r#"{"choices":[{"message":{"content":"The current time was retrieved successfully."}}],"model":"stub"}"#;

#[test]
fn stub_http_provider_completes_tool_loop() -> Result<()> {
    // Deterministic queue: round 0 tool call, round 1 tool call, round 2 text reply.
    let server = QueuedStub::new(vec![TOOL_CALL_RESPONSE, TOOL_CALL_RESPONSE, TEXT_RESPONSE]);
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

    // Deterministic wait: poll the Journal until the Receipt for this Run is
    // visible. deliver() is synchronous, but we poll defensively to surface
    // any ordering issue with a clear timeout instead of a silent assert fail.
    // The contract: by the time deliver() returns Ok, the tool-loop Receipts
    // for this run_id must be in the Journal.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let events = loop {
        let evs = journal.events()?;
        let has_receipt = evs.iter().any(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.run_id.as_ref() == Some(&outcome.run_id)
        });
        if has_receipt || std::time::Instant::now() > deadline {
            break evs;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    // ReceiptReceived for this Run, correlated with the Approved invocation.
    let receipt = events.iter().find(|e| {
        e.kind == JournalEventKind::ReceiptReceived && e.run_id.as_ref() == Some(&outcome.run_id)
    });
    assert!(
        receipt.is_some(),
        "tool execution receipt must be journaled for run {}",
        outcome.run_id.0
    );
    let receipt = receipt.unwrap();
    // The Receipt must share the correlation_id (invocation_id) with the
    // Approved fact — proving they refer to the same tool invocation.
    let approved = events.iter().find(|e| {
        e.kind == JournalEventKind::InvocationApproved && e.run_id.as_ref() == Some(&outcome.run_id)
    });
    assert!(
        approved.is_some(),
        "InvocationApproved must exist for run {}",
        outcome.run_id.0
    );
    assert_eq!(
        receipt.correlation_id,
        approved.unwrap().correlation_id,
        "Receipt correlation_id must match Approved correlation_id"
    );
    let llm_rounds = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::LlmCompleted && e.run_id.as_ref() == Some(&outcome.run_id)
        })
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
