//! Inline tests for the Phase 2 M2d follow-up HTTP endpoints
//! (`POST /v1/approve`, `POST /v1/deny`). Kept in a separate file so
//! `delivery_tests.rs` stays under the 500-line structure limit.

use super::*;
use crate::domain::*;
use serde_json::json;
use std::io::Read;
use std::sync::Arc;

/// Config with `require_write_approval = true` so a CLI Write reply pauses in
/// `AwaitingApproval`. Built inline (no shared helper exists server-side).
fn approval_config() -> crate::config::KernelConfig {
    use crate::config::KernelConfig;
    use std::path::PathBuf;
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
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec![],
        require_write_approval: true,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        capability_submit_token: None,
        capability_decision_token: None,
    }
}

/// Read the HTTP response body off a client stream (skips headers).
fn read_body(stream: &mut std::net::TcpStream) -> Value {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    while let Ok(n) = stream.read(&mut tmp) {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let body_text = text.split("\r\n\r\n").nth(1).unwrap_or("").trim();
    serde_json::from_str(body_text).unwrap_or_else(|_| json!({}))
}

/// Build a paused run and return (run_id, journal, gateway).
fn paused_run() -> anyhow::Result<(String, Arc<JournalStore>, Arc<crate::gateway::Gateway>)> {
    use crate::gateway::Gateway;
    use crate::llm::LocalEchoLlm;
    use crate::runtime::Runtime;
    let config = approval_config();
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let gateway = Arc::new(Gateway::new(config.clone()));
    let runtime = Runtime::new(config, LocalEchoLlm);
    let envelope = gateway.cli_ingress("hi".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert_eq!(
        journal.run_status(&outcome.run_id)?.as_deref(),
        Some("AwaitingApproval")
    );
    Ok((outcome.run_id.0, journal, gateway))
}

/// Drive `handle_approval_decision` over a real TCP round-trip; return the
/// parsed response body plus the journal (so the caller can assert run status).
fn run_decision(approved: bool) -> anyhow::Result<(Value, Arc<JournalStore>)> {
    let path = if approved { "/v1/approve" } else { "/v1/deny" };
    let (run_id, journal, gateway) = paused_run()?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let journal_c = Arc::clone(&journal);
    let gateway_c = Arc::clone(&gateway);
    let handle = std::thread::spawn(move || -> anyhow::Result<()> {
        let (mut conn, _) = listener.accept()?;
        let request = HttpRequest {
            method: "POST".to_string(),
            path: path.to_string(),
            bearer_token: Some("test-token".to_string()),
            body: serde_json::to_vec(&json!({ "run_id": run_id }))?,
        };
        handle_approval_decision(&mut conn, &gateway_c, &journal_c, &request, approved)
    });
    let mut client = std::net::TcpStream::connect(addr)?;
    handle.join().unwrap()?;
    Ok((read_body(&mut client), journal))
}

#[test]
fn approve_endpoint_resumes_paused_run_to_waiting_dispatch() -> anyhow::Result<()> {
    let (body, journal) = run_decision(true)?;
    assert_eq!(body.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        body.get("decision").and_then(|v| v.as_str()),
        Some("approved")
    );
    // The run advanced out of AwaitingApproval to WaitingDispatch. Find it by
    // scanning events for the run's RunStarted/ApprovalRequested correlation.
    let resumed = journal
        .events()?
        .iter()
        .any(|e| e.kind == JournalEventKind::ApprovalGranted);
    assert!(resumed, "ApprovalGranted fact must be journaled");
    Ok(())
}

#[test]
fn deny_endpoint_fails_paused_run() -> anyhow::Result<()> {
    let (body, journal) = run_decision(false)?;
    assert_eq!(body.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        body.get("decision").and_then(|v| v.as_str()),
        Some("denied")
    );
    let denied = journal
        .events()?
        .iter()
        .any(|e| e.kind == JournalEventKind::ApprovalDenied);
    assert!(denied, "ApprovalDenied fact must be journaled");
    Ok(())
}

#[test]
fn approve_endpoint_rejects_missing_run_id() -> anyhow::Result<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let journal = Arc::new(JournalStore::in_memory()?);
    journal.initialize_registry()?;
    let gateway = Arc::new(crate::gateway::Gateway::new(approval_config()));
    let journal_c = Arc::clone(&journal);
    let gateway_c = Arc::clone(&gateway);
    let handle = std::thread::spawn(move || -> anyhow::Result<()> {
        let (mut conn, _) = listener.accept()?;
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/approve".to_string(),
            bearer_token: Some("test-token".to_string()),
            body: serde_json::to_vec(&json!({})).unwrap_or_default(),
        };
        handle_approval_decision(&mut conn, &gateway_c, &journal_c, &request, true)
    });
    let mut client = std::net::TcpStream::connect(addr)?;
    handle.join().unwrap()?;
    let body = read_body(&mut client);
    assert_eq!(body.get("ok").and_then(|v| v.as_bool()), Some(false));
    assert!(body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .contains("missing run_id"));
    Ok(())
}
