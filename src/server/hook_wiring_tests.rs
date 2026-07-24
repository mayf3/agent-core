//! Production-path tests for context.prepare.v0 hook wiring.
//!
//! These tests exercise the full KernelConfig → Runtime → HttpHookClient
//! path via `deliver_event()`, the same function used in production.
//! They use a real TcpListener fake server to verify HTTP hook calls.

use super::*;
use crate::domain::*;
use crate::hook::{HookConfig, HookEndpoint, HookFailureMode, HookKind};
use chrono::Utc;
use serde_json::json;
use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};
use std::time::Duration;

// ── Helpers ─────────────────────────────────────────────────────────────

/// Build a minimal KernelConfig suitable for hook wiring tests.
fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        data_dir: std::path::PathBuf::from(".agent-core-test"),
        agent_id: crate::domain::AgentId("main".to_string()),
        root_dir: std::path::PathBuf::from("."),
        kernel_port: 0,
        connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: String::new(),
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
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        coding_harness_api_url: "http://127.0.0.1:7200".into(),
        coding_harness_artifact_digest:
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: HookConfig::default(),
    }
}

/// Build a valid HookResponseEnvelope JSON body containing a single fragment.
fn response_body_with_fragment(text: &str) -> String {
    let prepare_resp = json!({
        "fragments": [{
            "id": "f1",
            "hook_id": "context.prepare.v0",
            "kind": "instruction",
            "placement": "user_context",
            "priority": 1,
            "content": text,
            "source": "hook:test",
            "ttl_secs": null,
            "estimated_tokens": 10,
            "sensitivity": "public",
        }],
        "resource_refs": [],
    });
    let envelope = json!({
        "request_id": "test-rid",
        "hook": "context.prepare.v0",
        "timestamp": Utc::now().to_rfc3339(),
        "payload": prepare_resp,
    });
    serde_json::to_string(&envelope).unwrap()
}

/// Spawn a minimal TcpListener that responds with the given body and status
/// for every request. Mirrors the pattern in `http_tests.rs`. The caller
/// waits on the barrier before proceeding to ensure the server is accepting.
fn spawn_fake_hook_with_barrier(
    body: &str,
    status: u16,
    barrier: Arc<Barrier>,
) -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let request_count = Arc::new(AtomicUsize::new(0));
    let count = request_count.clone();
    let body = body.to_string();
    std::thread::spawn(move || {
        // Signal that the thread has started and the listener is bound.
        barrier.wait();
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            count.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let reason = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, request_count)
}

/// Convenience wrapper that spawns a fake hook and waits for readiness.
fn spawn_fake_hook(body: &str, status: u16) -> (u16, Arc<AtomicUsize>) {
    let barrier = Arc::new(Barrier::new(2));
    let b = Arc::clone(&barrier);
    let (port, count) = spawn_fake_hook_with_barrier(body, status, b);
    barrier.wait();
    // Brief safety sleep to let the thread settle on incoming().
    std::thread::sleep(Duration::from_millis(10));
    (port, count)
}

/// Run a full delivery cycle through `deliver_event` and return the journal.
fn run_delivery(config: KernelConfig) -> Result<JournalStore> {
    let journal = JournalStore::in_memory()?;
    journal.initialize_registry()?;
    let gateway = Gateway::new(config.clone());
    let envelope = gateway.cli_ingress("hello".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    deliver_event(config, &journal, &gateway, event)?;
    Ok(journal)
}

// ── Test 1: Default disabled ────────────────────────────────────────────

#[test]
fn hook_default_disabled_no_call() -> Result<()> {
    // Default config has hook disabled. The production path in `deliver_event`
    // does NOT call `with_hook()` when `enabled` is false.
    let config = test_config();
    assert!(!config.context_prepare_hook.enabled);
    let journal = run_delivery(config)?;
    let events = journal.events()?;
    assert!(
        !events
            .iter()
            .any(|e| e.kind == JournalEventKind::HookCallRecorded),
        "no HookCallRecorded when hook is disabled"
    );
    // Verify hash chain integrity.
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ── Test 2: Enabled + fake HTTP server ──────────────────────────────────

#[test]
fn hook_enabled_fake_server_injects_fragment() -> Result<()> {
    let fragment_text = "EXTERNAL_CONTEXT_SMOKE_WORD: papaya";
    let response_body = response_body_with_fragment(fragment_text);
    let (port, request_count) = spawn_fake_hook(&response_body, 200);

    let mut config = test_config();
    config.context_prepare_hook = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: format!("http://127.0.0.1:{port}"),
        },
        failure_mode: HookFailureMode::FailOpen,
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        max_fragments: 10,
    };

    let journal = run_delivery(config)?;

    // Fake server received exactly 1 request.
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        1,
        "fake server must receive exactly one POST"
    );

    // Journal has HookCallRecorded with status=ok.
    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .expect("HookCallRecorded must exist");
    let status = rec.payload.get("status").and_then(|v| v.as_str());
    let error_code = rec
        .payload
        .get("error_code")
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");
    assert_eq!(
        status,
        Some("ok"),
        "expected status=ok, got {status:?}, error_code={error_code:?}"
    );
    assert_eq!(
        rec.payload.get("hook").and_then(|v| v.as_str()),
        Some("context.prepare.v0")
    );
    assert!(rec.payload.get("fragment_count").is_some());
    assert!(rec.payload.get("duration_ms").is_some());

    // Verify hash chain integrity.
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ── Test 3: Enabled but empty URL ───────────────────────────────────────

#[test]
fn hook_enabled_empty_url_endpoint_missing() -> Result<()> {
    let mut config = test_config();
    config.context_prepare_hook = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: String::new(), // empty → no network
        },
        failure_mode: HookFailureMode::FailOpen,
        ..Default::default()
    };

    let journal = run_delivery(config)?;
    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .expect("HookCallRecorded must exist");
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("skipped")
    );
    let error_code = rec
        .payload
        .get("error_code")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        error_code.contains("endpoint_missing"),
        "error_code should contain 'endpoint_missing', got: {error_code:?}"
    );
    // Verify hash chain integrity.
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ── Test 4: FailOpen on HTTP 500 ────────────────────────────────────────

#[test]
fn hook_fail_open_on_http_500() -> Result<()> {
    let (port, request_count) = spawn_fake_hook("internal error", 500);

    let mut config = test_config();
    config.context_prepare_hook = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: format!("http://127.0.0.1:{port}"),
        },
        failure_mode: HookFailureMode::FailOpen,
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        max_fragments: 10,
    };

    let journal = run_delivery(config)?;

    // Fake server received a request.
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    // Delivery succeeded despite hook failure.
    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .expect("HookCallRecorded must exist");
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("skipped"),
        "fail_open should record status=skipped"
    );
    assert_eq!(
        rec.payload.get("failure_mode").and_then(|v| v.as_str()),
        Some("fail_open")
    );

    // No RunFailed event — the run continued.
    assert!(
        !events.iter().any(|e| e.kind == JournalEventKind::RunFailed),
        "fail_open must not produce RunFailed"
    );

    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ── Test 5: FailClosed on HTTP 500 ──────────────────────────────────────

#[test]
fn hook_fail_closed_on_http_500() -> Result<()> {
    let (port, request_count) = spawn_fake_hook("internal error", 500);

    let mut config = test_config();
    config.context_prepare_hook = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: format!("http://127.0.0.1:{port}"),
        },
        failure_mode: HookFailureMode::FailClosed,
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        max_fragments: 10,
    };

    let journal = run_delivery(config)?;

    // Fake server received a request.
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    // Delivery returned Ok (reply_with_failure returns Ok(RuntimeOutcome)).
    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .expect("HookCallRecorded must exist");
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("failed"),
        "fail_closed should record status=failed"
    );
    assert_eq!(
        rec.payload.get("failure_mode").and_then(|v| v.as_str()),
        Some("fail_closed")
    );

    // RunFailed event exists.
    assert!(
        events.iter().any(|e| e.kind == JournalEventKind::RunFailed),
        "fail_closed must produce RunFailed"
    );

    assert!(journal.verify_hash_chain()?);
    Ok(())
}
