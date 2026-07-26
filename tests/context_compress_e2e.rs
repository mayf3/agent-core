//! End-to-end test: Kernel + real external simple-compactor Provider process.
//!
//! Starts the simple-compactor binary, configures Kernel HookClient to it,
//! runs a multi-tool agent loop, verifies the full chain.

use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::hook::{HookClient, HookConfig, HookKind, HttpHookClient, HookEndpoint, HookFailureMode};
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use agent_core_kernel::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct SimpleCompactorProcess {
    child: Child,
    port: u16,
}

impl SimpleCompactorProcess {
    fn start() -> Self {
        let binary = find_binary("context-simple-compactor");
        let port = 18702u16;
        let mut child = Command::new(&binary)
            .env("SIMPLECOMPACTOR_PORT", port.to_string())
            .spawn()
            .unwrap_or_else(|_| panic!("failed to start {binary} at port {port}"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                break;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                panic!("simple-compactor did not become ready on port {port}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("simple-compactor ready on port {port}");
        Self { child, port }
    }
    fn endpoint(&self) -> String { format!("http://127.0.0.1:{}", self.port) }
}

impl Drop for SimpleCompactorProcess {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

fn find_binary(name: &str) -> String {
    for dir in &["target/release", "target/debug"] {
        let p = format!("{dir}/{name}");
        if std::path::Path::new(&p).exists() { return p; }
    }
    panic!("binary {name} not found (build: cargo build --release -p {name})");
}

struct MultiToolLlm { round: Arc<Mutex<usize>> }

impl LlmClient for MultiToolLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        let mut round = self.round.lock().unwrap();
        let current = *round;
        *round += 1;
        let (content, tool_call) = match current {
            0 => ("checking system".into(), ToolCallResult::Valid(ToolCall {
                id: agent_core_kernel::llm::tool_call_id_hash("tool_r0"),
                operation: "system.status".into(), arguments: json!({}),
            })),
            1 => ("checking again".into(), ToolCallResult::Valid(ToolCall {
                id: agent_core_kernel::llm::tool_call_id_hash("tool_r1"),
                operation: "system.status".into(), arguments: json!({}),
            })),
            _ => ("all done, system is healthy.".into(), ToolCallResult::Absent),
        };
        Ok(LlmOutput { provider: "test".into(), model: "multi-tool".into(),
            content, journal_payload: json!({"round": current}),
            tool_call, provider_turn: None })
    }
}

#[test]
fn context_compress_e2e_with_external_provider() -> Result<()> {
    let _provider = SimpleCompactorProcess::start();
    let hook_cfg = HookConfig {
        enabled: true, kind: HookKind::ContextCompressV0,
        endpoint: HookEndpoint { url: _provider.endpoint() },
        timeout_ms: 10_000, max_request_bytes: 1048576, max_response_bytes: 1048576,
        max_fragments: 20, failure_mode: HookFailureMode::FailOpen,
    };
    let hook_client: Box<dyn HookClient> = Box::new(HttpHookClient::new());
    let mut config = common::test_config();
    config.max_tool_rounds = 4;
    let runtime = Runtime::new(config.clone(), MultiToolLlm { round: Arc::new(Mutex::new(0)) })
        .with_hook(hook_client, hook_cfg);
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let envelope = gateway.cli_ingress("check system status".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    let events = journal.events()?;
    let re: Vec<_> = events.iter().filter(|e| e.run_id.as_ref() == Some(&outcome.run_id)).collect();

    let model_completions = re.iter().filter(|e| e.kind == JournalEventKind::LlmCompleted).count();
    let compress_calls: Vec<_> = re.iter().filter(|e| {
        e.kind == JournalEventKind::HookCallRecorded
        && e.payload.get("hook").and_then(|v| v.as_str()) == Some("context.compress.v0")
    }).collect();
    let tool_receipts: Vec<_> = re.iter().filter(|e| {
        e.kind == JournalEventKind::ReceiptReceived
        && e.payload.get("status").and_then(|v| v.as_str()) == Some("Succeeded")
    }).collect();

    // Analyze compress call modes
    let passthrough_count = compress_calls.iter()
        .filter(|e| e.payload.get("mode").and_then(|v| v.as_str()) == Some("passthrough")).count();
    let compacted_count = compress_calls.iter()
        .filter(|e| e.payload.get("mode").and_then(|v| v.as_str()) == Some("compacted")).count();

    // Capture digests from compacted calls
    let compacted_digests: Vec<&str> = compress_calls.iter()
        .filter(|e| e.payload.get("mode").and_then(|v| v.as_str()) == Some("compacted"))
        .filter_map(|e| e.payload.get("plan_digest").and_then(|v| v.as_str()))
        .collect();

    eprintln!("MODEL_COMPLETIONS={} COMPRESS_CALLS={} PASSTHROUGH={} COMPACTED={}",
        model_completions, compress_calls.len(), passthrough_count, compacted_count);
    for e in &compress_calls {
        eprintln!("  COMPRESS payload={:?}", e.payload);
    }

    // Every tool call succeeded
    for r in &tool_receipts {
        assert_eq!(r.payload.get("status").and_then(|v| v.as_str()), Some("Succeeded"));
    }
    assert!(tool_receipts.len() >= 2, ">=2 tool receipts, got {}", tool_receipts.len());
    assert!(compress_calls.len() >= model_completions,
        "at least as many compress calls ({}) as model completions ({})",
        compress_calls.len(), model_completions);
    // Must have at least one compacted round
    assert!(compacted_count >= 1, "at least 1 compacted compress call, got {compacted_count}");

    // Print compacted evidence
    for e in &compress_calls {
        if e.payload.get("mode").and_then(|v| v.as_str()) == Some("compacted") {
            eprintln!("  COMPACTED_CALL seq={} plan_digest={} estimated_size={:?}",
                e.sequence,
                e.payload.get("plan_digest").and_then(|v| v.as_str()).unwrap_or("?"),
                e.payload.get("estimated_size").and_then(|v| v.as_u64()));
        }
    }

    println!("=== E2E Evidence ===");
    println!("RUN_ID={}", outcome.run_id.0);
    for e in &re {
        let hook_val = e.payload.get("hook").and_then(|v| v.as_str()).unwrap_or("");
        let mode_val = e.payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
        println!("  seq={} kind={:?} hook={} mode={} op={} status={}",
            e.sequence, e.kind, hook_val, mode_val,
            e.payload.get("operation").and_then(|v| v.as_str()).unwrap_or(""),
            e.payload.get("status").and_then(|v| v.as_str()).unwrap_or(""));
    }
    println!("MODEL_COMPLETIONS={model_completions}");
    println!("COMPRESS_CALLS={}", compress_calls.len());
    println!("PASSTHROUGH_COUNT={passthrough_count}");
    println!("COMPACTED_COUNT={compacted_count}");
    if !compacted_digests.is_empty() {
        println!("COMPACTED_PLAN_DIGEST={}", compacted_digests[0]);
        if compacted_digests.len() > 1 {
            println!("POST_COMPACTED_PLAN_DIGEST={}", compacted_digests[1]);
        }
    }
    println!("TOOL_RECEIPTS={}", tool_receipts.len());
    Ok(())
}

mod common;
