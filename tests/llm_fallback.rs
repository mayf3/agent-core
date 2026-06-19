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
    let llm = OpenAiCompatibleLlm::new(
        primary,
        "primary-key".to_string(),
        "bad-primary".to_string(),
        2_000,
    )
    .with_fallback(
        fallback,
        "fallback-key".to_string(),
        "deepseek-v4-flash".to_string(),
    );

    let output = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "hello".to_string(),
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
            let status_text = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Ok(format!("http://{addr}/v1"))
}

fn read_http_request(stream: &mut TcpStream) -> Result<()> {
    let mut buffer = [0_u8; 2048];
    let _ = stream.read(&mut buffer)?;
    Ok(())
}

mod common;

use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmOutput, ToolCall};
use agent_core_kernel::runtime::Runtime;
use std::sync::{Arc, Mutex};

// ---- Session Recall Loop (Task 1) ----
//
// These tests cover the recall loop added to `Runtime::deliver`: when the first
// LLM round emits a read-only tool call, the Runtime executes it inline,
// appends the result as a `ToolResult` context block, and re-invokes the LLM so
// it can fold the tool output into its reply. The loop is bounded by
// `MAX_TOOL_ROUNDS` (== 2). Tool failures are fed back as ToolResult blocks
// rather than crashing the run.

/// LLM that proposes `session.recall_recent` on round 0 and replies with the
/// recalled text on round 1. Verifies the second-round reply is used.
struct RecallThenAnswerLlm {
    round: Arc<Mutex<usize>>,
    saw_tool_result_block: Arc<Mutex<bool>>,
}

impl LlmClient for RecallThenAnswerLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        let mut round = self.round.lock().unwrap();
        let current = *round;
        *round += 1;
        // Detect whether the Runtime fed back a ToolResult block (round >= 1).
        if current >= 1 {
            let saw = input
                .blocks
                .iter()
                .any(|b| matches!(b.kind, ContextBlockKind::ToolResult));
            *self.saw_tool_result_block.lock().unwrap() = saw;
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

    // The final reply must be the SECOND round's text (which folds in the
    // recalled content), not the first round's "let me recall".
    assert!(
        outcome.output.contains("WaitingDispatch"),
        "final reply should come from the second LLM round, got: {}",
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

    // The LLM observed a ToolResult context block on its second call.
    assert!(
        *saw.lock().unwrap(),
        "second LLM round must receive a ToolResult context block"
    );
    Ok(())
}

/// LLM that ALWAYS proposes `session.recall_recent`, no matter the round. Used
/// to verify `MAX_TOOL_ROUNDS` caps the loop — the tool must not run forever.
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

    // MAX_TOOL_ROUNDS == 2: the LLM is invoked at most 3 times total
    // (1 initial + up to 2 recall rounds). The exact constant is private to the
    // Runtime, so we assert the loop terminates with a bounded, small number of
    // LLM calls and that the run does not hang or panic.
    let llm_completed = journal
        .events()?
        .iter()
        .filter(|e| {
            e.run_id.as_ref() == Some(&outcome.run_id) && e.kind == JournalEventKind::LlmCompleted
        })
        .count();
    assert!(
        llm_completed <= 3,
        "recall loop must be bounded; saw {} LlmCompleted events",
        llm_completed
    );
    assert!(
        llm_completed >= 2,
        "recall loop must perform at least the initial + one recall round; saw {}",
        llm_completed
    );
    // The run still completes normally and produces a reply.
    assert!(!outcome.output.is_empty());
    Ok(())
}

/// LLM whose first-round tool call targets an operation that the inline
/// executor reports as not-implemented (`other =>` arm in
/// `handle_inline_tool_call`). Verifies the error is fed back as a ToolResult
/// and the run does not crash.
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
            let saw_error = input.blocks.iter().any(|b| {
                matches!(b.kind, ContextBlockKind::ToolResult) && b.content.contains("error")
            });
            *self.saw_error_block.lock().unwrap() = saw_error;
            return Ok(LlmOutput {
                provider: "test".into(),
                model: "unknown-tool".into(),
                content: "sorry, that tool is unavailable".into(),
                journal_payload: json!({ "round": current }),
                tool_call: None,
            });
        }
        // Propose a deliberately uncatalogued name to hit the rejection branch
        // in validate_tool_call (unknown_operation), which handle_inline_tool_call
        // converts into a rejected ToolResult rather than crashing.
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

    // The run must complete without panicking and produce a graceful reply.
    assert!(
        outcome.output.contains("unavailable"),
        "model should recover from the rejected tool call, got: {}",
        outcome.output
    );
    assert!(
        *saw_error.lock().unwrap(),
        "the rejected tool call must be fed back as a ToolResult block"
    );
    Ok(())
}

#[test]
fn recall_loop_is_noop_when_no_tool_call() -> Result<()> {
    // Backwards-compatibility: when the LLM emits no tool call, the loop must
    // be a no-op and the original first-round reply is used unchanged.
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
    // Exactly one LlmCompleted event (no extra recall round).
    let llm_completed = journal
        .events()?
        .iter()
        .filter(|e| {
            e.run_id.as_ref() == Some(&outcome.run_id) && e.kind == JournalEventKind::LlmCompleted
        })
        .count();
    assert_eq!(llm_completed, 1, "no tool call → exactly one LLM round");
    Ok(())
}
