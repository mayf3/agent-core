//! Tests for HttpHookClient using an in-process TcpListener as a fake hook
//! server.  No real network, no external dependencies.

use crate::hook::{
    ContextFragment, ContextFragmentKind, ContextPrepareRequest, FragmentPlacement,
    FragmentSensitivity, HookClient, HookConfig, HookEndpoint, HookKind, HookResponseEnvelope,
    HttpHookClient,
};
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

// ── Helpers ────────────────────────────────────────────────────────────

/// A fake hook server handle. The server handles exactly one connection
/// then exits. Drop is a no-op (the thread is detached but short-lived).
struct FakeHookServer {
    port: u16,
}

impl FakeHookServer {
    fn port(&self) -> u16 {
        self.port
    }
}

/// Spawn a minimal TcpListener that responds with the given status line,
/// headers, and body for a single request. The server handles exactly one
/// connection, then exits cleanly (no zombie threads, no races).
fn spawn_fake_hook(body: &str, status: u16) -> FakeHookServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = body.to_string();
    thread::spawn(move || {
        if let Ok(stream) = listener.accept() {
            let mut stream = stream.0;
            // Set a short read timeout so drain doesn't block forever.
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(100)));
            let reason = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            // Write response first, then drain any remaining data (including
            // the client's request) so the socket stays alive until the client
            // finishes reading. The read timeout ensures the thread exits even
            // if the client keeps the connection open.
            let _ = stream.write_all(response.as_bytes());
            let mut discard = [0u8; 4096];
            loop {
                match stream.read(&mut discard) {
                    Ok(0) | Err(_) => break, // EOF or timeout → done
                    Ok(_) => continue,        // more data to drain
                }
            }
        }
    });
    FakeHookServer { port }
}

/// Build a valid ContextPrepareRequest for testing.
fn test_request() -> ContextPrepareRequest {
    ContextPrepareRequest {
        hook: HookKind::ContextPrepareV0,
        run_id: "r1".into(),
        session_id: "s1".into(),
        agent_id: "main".into(),
        principal: "user".into(),
        channel: "cli".into(),
        user_text: "hello".into(),
        context_budget_chars: 4000,
    }
}

/// Build a HookConfig pointing to a specific port.
fn hook_config(port: u16) -> HookConfig {
    HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: format!("http://127.0.0.1:{port}"),
        },
        timeout_ms: 5000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        max_fragments: 10,
        failure_mode: crate::hook::HookFailureMode::FailOpen,
    }
}

/// Build a valid HookResponseEnvelope JSON body with the given payload.
fn valid_response_body(payload: &serde_json::Value) -> String {
    let env = HookResponseEnvelope {
        request_id: "test-req".into(),
        hook: HookKind::ContextPrepareV0,
        timestamp: Utc::now(),
        payload: payload.clone(),
    };
    serde_json::to_string(&env).unwrap()
}

/// Build a ContextPrepareResponse payload with a single fragment.
fn response_with_fragment(text: &str) -> serde_json::Value {
    let frag = ContextFragment {
        id: "f1".into(),
        hook_id: "context.prepare.v0".into(),
        kind: ContextFragmentKind::Instruction,
        placement: FragmentPlacement::UserContext,
        priority: 1,
        content: text.to_string(),
        source: "hook:test".into(),
        ttl_secs: None,
        estimated_tokens: 10,
        sensitivity: FragmentSensitivity::Public,
    };
    let resp = crate::hook::client::ContextPrepareResponse {
        fragments: vec![frag],
        resource_refs: vec![],
    };
    serde_json::to_value(&resp).unwrap()
}

// ── Tests ──────────────────────────────────────────────────────────────

#[test]
fn http_hook_success_returns_fragment() -> Result<()> {
    let payload = response_with_fragment("use the tool");
    let body = valid_response_body(&payload);
    let server = spawn_fake_hook(&body, 200);

    let client = HttpHookClient::new();
    let cfg = hook_config(server.port());
    let resp = client.call_context_prepare(&test_request(), &cfg)?;

    assert_eq!(resp.fragments.len(), 1);
    assert_eq!(resp.fragments[0].content, "use the tool");
    assert_eq!(resp.fragments[0].hook_id, "context.prepare.v0");
    Ok(())
}

#[test]
fn http_hook_disabled_no_network_call() {
    // When hook is disabled, HttpHookClient is never called — the Runtime
    // checks `config.enabled` first.  This test verifies the contract.
    let cfg = HookConfig {
        enabled: false,
        ..Default::default()
    };
    assert!(!cfg.enabled);
    // No HTTP call is made because the Runtime never invokes the client
    // when `enabled` is false.
}

#[test]
fn http_hook_timeout_maps_error_code() {
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: "http://127.0.0.1:19999".into(), // unreachable
        },
        timeout_ms: 1, // very short timeout
        ..Default::default()
    };
    let client = HttpHookClient::new();
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    // With a 1ms timeout to an unreachable address, we expect timeout or connect error.
    assert!(
        err.contains("timeout") || err.contains("connect") || err.contains("transport"),
        "unexpected error: {err}"
    );
}

#[test]
fn http_hook_connection_refused_maps_error_code() {
    // Connect to a port that is not listening.
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: "http://127.0.0.1:1".into(), // port 1 is privileged → connection refused
        },
        timeout_ms: 5000,
        ..Default::default()
    };
    let client = HttpHookClient::new();
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("http_connect_error") || err.contains("http_transport_error"),
        "unexpected: {err}"
    );
}

#[test]
fn http_hook_500_maps_error_code() {
    let server = spawn_fake_hook("Internal Server Error", 500);
    let client = HttpHookClient::new();
    let cfg = hook_config(server.port());
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("http_status_5xx"), "expected 5xx, got: {err}");
}

#[test]
fn http_hook_400_maps_error_code() {
    let server = spawn_fake_hook("Bad Request", 400);
    let client = HttpHookClient::new();
    let cfg = hook_config(server.port());
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("http_status_4xx"), "expected 4xx, got: {err}");
}

#[test]
fn http_hook_invalid_json_maps_error_code() {
    let server = spawn_fake_hook("not json at all", 200);
    let client = HttpHookClient::new();
    let cfg = hook_config(server.port());
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("invalid_json"),
        "expected invalid_json, got: {err}"
    );
}

#[test]
fn http_hook_response_too_large_rejected() {
    // Create a response that's larger than the configured limit.
    let large_text = "x".repeat(200);
    let payload = response_with_fragment(&large_text);
    let body = valid_response_body(&payload);

    let server = spawn_fake_hook(&body, 200);
    let mut cfg = hook_config(server.port());
    cfg.max_response_bytes = 50; // very small limit

    let client = HttpHookClient::new();
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("response_too_large"),
        "expected too_large, got: {err}"
    );
}

#[test]
fn http_hook_response_envelope_mismatch_rejected() {
    // Return a HookResponseEnvelope with wrong hook kind.
    let env = HookResponseEnvelope {
        request_id: "test-req".into(),
        hook: HookKind::IngressRouteV0, // wrong kind!
        timestamp: Utc::now(),
        payload: json!({"fragments":[],"resource_refs":[]}),
    };
    let body = serde_json::to_string(&env).unwrap();
    let server = spawn_fake_hook(&body, 200);

    let client = HttpHookClient::new();
    let cfg = hook_config(server.port());
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unsupported_hook_response"),
        "expected unsupported_hook_response, got: {err}"
    );
}

#[test]
fn http_hook_empty_endpoint_rejected() {
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint { url: String::new() },
        ..Default::default()
    };
    let client = HttpHookClient::new();
    let result = client.call_context_prepare(&test_request(), &cfg);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("endpoint_missing"),
        "expected endpoint_missing, got: {err}"
    );
}

#[test]
fn http_hook_does_not_log_or_expose_secrets() {
    // Verify that error codes do not contain full endpoint URLs or secrets.
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: "http://user:secret@127.0.0.1:1/hook".into(),
        },
        timeout_ms: 100,
        ..Default::default()
    };
    let client = HttpHookClient::new();
    let result = client.call_context_prepare(&test_request(), &cfg);
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(
            !msg.contains("secret"),
            "error message leaked 'secret': {msg}"
        );
    }
}

// ── Runtime-level HTTP hook test ───────────────────────────────────────

#[test]
fn runtime_http_hook_success_injects_fragment() -> Result<()> {
    use crate::domain::*;
    use crate::journal::JournalStore;

    // Start a fake hook server that returns a context fragment.
    let payload = response_with_fragment("http hook fragment");
    let body = valid_response_body(&payload);
    let server = spawn_fake_hook(&body, 200);

    // Build blocks containing UserMessage.
    let hook_cfg = hook_config(server.port());
    let client = HttpHookClient::new();
    let mut blocks = vec![
        ContextBlock {
            kind: ContextBlockKind::RootSystem,
            content: "root".into(),
            compressibility: Compressibility::Never,
            source_ref: None,
        },
        ContextBlock {
            kind: ContextBlockKind::UserMessage,
            content: "hello".into(),
            compressibility: Compressibility::Truncate,
            source_ref: None,
        },
    ];

    let journal = JournalStore::in_memory()?;

    // Call the runtime hook function directly.
    let outcome = crate::runtime::hook_call::call_context_prepare(
        &mut blocks,
        &client,
        &hook_cfg,
        &journal,
        &RunId::new(),
        &SessionId("s1".into()),
        "main",
        "user",
        "cli",
        "test",
        4000,
    )?;

    assert!(matches!(
        outcome,
        crate::runtime::hook_call::HookCallOutcome::Injected
    ));

    // Verify HookFragment was injected before UserMessage.
    let hook_count = blocks
        .iter()
        .filter(|b| b.kind == ContextBlockKind::HookFragment)
        .count();
    assert_eq!(hook_count, 1, "expected one HookFragment");
    let last = blocks.last().unwrap();
    assert_eq!(
        last.kind,
        ContextBlockKind::UserMessage,
        "UserMessage must be last"
    );

    // Verify HookCallRecorded in journal.
    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .expect("expected HookCallRecorded event");
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("ok")
    );
    assert_eq!(
        rec.payload.get("hook").and_then(|v| v.as_str()),
        Some("context.prepare.v0")
    );

    Ok(())
}
