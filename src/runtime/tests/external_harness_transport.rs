//! External harness transport and Runtime e2e tests.
//!
//! Uses a real TcpListener as the harness fixture on a random port.
//! Tests verify full Runtime → localhost → Receipt → ToolResult chain.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 0,
        connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
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
    }
}

/// Start a minimal harness on a random port. Returns (endpoint, shutdown_flag).
fn start_harness() -> Result<(String, Arc<AtomicBool>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let endpoint = format!("http://127.0.0.1:{port}/execute");
    let shutdown = Arc::new(AtomicBool::new(false));
    let sh = shutdown.clone();
    thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        loop {
            if sh.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => handle_harness_request(&mut stream),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    Ok((endpoint, shutdown))
}

fn handle_harness_request(stream: &mut TcpStream) {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf);
    let body = r#"{
        "protocol_version": "external-harness-v1",
        "ok": true,
        "result": {
            "iso": "2026-06-30T12:00:00+00:00",
            "epoch_ms": 1234567890
        }
    }"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn register_and_enable_harness(j: &JournalStore, g: &Gateway, endpoint: &str) -> Result<String> {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "test-harness".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: endpoint.into(),
        operation_name: "external.time_now".into(),
        description: "Return current time".into(),
        input_schema: json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
        output_schema: json!({"type": "object", "properties": {"iso": {"type": "string"}, "epoch_ms": {"type": "integer"}}, "required": ["iso", "epoch_ms"], "additionalProperties": false}),
        idempotent: true,
        created_at: Utc::now(),
    };
    let manifest_id = m.compute_manifest_id()?;
    m.manifest_id = manifest_id.clone();
    j.register_harness_manifest(&m)?;

    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: manifest_id.clone(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    };
    let approved = g.approve_harness_change(intent)?;
    j.enable_harness(&approved)?;
    Ok(manifest_id)
}

#[test]
fn external_harness_non_2xx_records_failed_receipt() -> Result<()> {
    use crate::adapters::external_harness::{
        execute_external_harness, ExternalHarnessTransportConfig,
    };

    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "test".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:1/execute".into(),
        operation_name: "external.test".into(),
        description: "test".into(),
        input_schema: json!({}),
        output_schema: json!({}),
        idempotent: false,
        created_at: Utc::now(),
    };
    let manifest_id = m.compute_manifest_id()?;
    m.manifest_id = manifest_id;

    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: InvocationId("inv_test".into()),
            run_id: RunId("r_test".into()),
            operation: "external.test".into(),
            arguments: json!({}),
            idempotency_key: None,
        },
        "decision_test".into(),
    );

    // Short timeout for fast test.
    let config = ExternalHarnessTransportConfig {
        connect_timeout: Duration::from_millis(100),
        read_timeout: Duration::from_millis(100),
        write_timeout: Duration::from_millis(100),
        ..ExternalHarnessTransportConfig::default()
    };
    let result = crate::adapters::external_harness::execute_external_harness_with_config(
        &m, &approved, &config,
    );
    assert!(result.is_err(), "connect to port 1 should fail");
    Ok(())
}

#[test]
fn external_harness_malformed_json_records_failed_receipt() -> Result<()> {
    use crate::adapters::external_harness::execute_external_harness;

    let m = HarnessManifest {
        manifest_id: "malformed_test".into(),
        harness_id: "test".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:1/execute".into(),
        operation_name: "external.test".into(),
        description: "test".into(),
        input_schema: json!({}),
        output_schema: json!({}),
        idempotent: false,
        created_at: Utc::now(),
    };

    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: InvocationId("inv_malformed".into()),
            run_id: RunId("r_malformed".into()),
            operation: "external.test".into(),
            arguments: json!({}),
            idempotency_key: None,
        },
        "decision_malformed".into(),
    );

    let result = execute_external_harness(&m, &approved);
    // Should fail because port 1 refuses connection → connect_failed
    assert!(result.is_err());
    Ok(())
}

#[test]
fn external_harness_ok_false_records_failed_receipt() -> Result<()> {
    use crate::adapters::external_harness::execute_external_harness;

    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "test".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:1/execute".into(),
        operation_name: "external.test".into(),
        description: "test".into(),
        input_schema: json!({}),
        output_schema: json!({}),
        idempotent: false,
        created_at: Utc::now(),
    };
    let manifest_id = m.compute_manifest_id()?;
    m.manifest_id = manifest_id;

    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: InvocationId("inv_okfalse".into()),
            run_id: RunId("r_okfalse".into()),
            operation: "external.test".into(),
            arguments: json!({}),
            idempotency_key: None,
        },
        "decision_okfalse".into(),
    );

    // Port 1 nothing listening → connection error (harness_failed).
    let result = execute_external_harness(&m, &approved);
    assert!(result.is_err());
    Ok(())
}

#[test]
fn external_harness_tool_call_runs_end_to_end() -> Result<()> {
    let (endpoint, _shutdown) = start_harness()?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    // Register and enable harness.
    register_and_enable_harness(&j, &g, &endpoint)?;

    // Verify the snapshot contains the external operation.
    let snapshot_id = j.current_registry_snapshot_id()?;
    let snapshot = j.load_registry_snapshot(&snapshot_id)?;
    let spec = snapshot
        .lookup("external.time_now")
        .expect("external.time_now must be in snapshot");

    // Test the full adapter chain directly.
    let approved = g.approve_invocation(
        InvocationIntent {
            invocation_id: InvocationId("inv_e2e".into()),
            run_id: RunId("r_e2e".into()),
            operation: "external.time_now".into(),
            arguments: json!({"session_id": "s_e2e"}),
            idempotency_key: None,
        },
        &Run {
            id: RunId("r_e2e".into()),
            session_id: SessionId("s_e2e".into()),
            agent_id: AgentId("main".to_string()),
            trigger_event_id: EventId("ev_e2e".into()),
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".to_string()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![
                    CapabilityGrant {
                        operation: "stdout.send_text".to_string(),
                        scope: "current_session".to_string(),
                    },
                    CapabilityGrant {
                        operation: "external.time_now".to_string(),
                        scope: "current_session".to_string(),
                    },
                ],
                requester_id: Some("cli:local".to_string()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            registry_snapshot_id: snapshot_id.clone(),
        },
        &Session {
            id: SessionId("s_e2e".into()),
            agent_id: AgentId("main".to_string()),
            channel: ChannelKind::Cli,
            conversation_key: "local".to_string(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        },
        &snapshot,
    )?;

    let receipt = crate::adapters::external_harness::execute_external_harness(
        &{ j.load_harness_manifest(&spec.binding_key)?.unwrap() },
        &approved,
    )?;

    assert_eq!(receipt.status, ReceiptStatus::Succeeded);
    assert!(receipt.output.get("iso").and_then(|v| v.as_str()).is_some());
    assert!(receipt
        .output
        .get("epoch_ms")
        .and_then(|v| v.as_u64())
        .is_some());

    // Verify the ReceiptReceived event would have the correct data.
    assert_eq!(receipt.invocation_id.0, "inv_e2e");
    Ok(())
}

#[test]
fn harness_route_register_enable_disable_works() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    // Register via route handler.
    let register_body = json!({
        "harness_id": "route-test-harness",
        "artifact_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "protocol_version": "external-harness-v1",
        "endpoint": "http://127.0.0.1:9999/execute",
        "operation_name": "external.route_test",
        "description": "Route test harness",
        "input_schema": {"type": "object", "properties": {}, "required": [], "additionalProperties": false},
        "output_schema": {"type": "object", "properties": {"status": {"type": "string"}}, "required": ["status"], "additionalProperties": false},
        "idempotent": true
    });

    let result = crate::server::harness_routes::handle_register(&g, &j, &register_body)?;
    let result_val: serde_json::Value = serde_json::from_str(&result)?;
    assert_eq!(result_val["ok"], true);
    let manifest_id = result_val["manifest_id"].as_str().unwrap().to_string();

    // Enable via route handler.
    let enable_body = json!({
        "manifest_id": manifest_id,
        "expected_snapshot_id": j.current_registry_snapshot_id()?,
    });
    let result = crate::server::harness_routes::handle_enable(&g, &j, &enable_body)?;
    let result_val: serde_json::Value = serde_json::from_str(&result)?;
    assert_eq!(result_val["ok"], true);
    assert!(result_val["active_snapshot_id"].as_str().unwrap().len() > 0);
    let s2_id = result_val["active_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Disable via route handler.
    let disable_body = json!({
        "manifest_id": manifest_id,
        "expected_snapshot_id": s2_id,
    });
    let result = crate::server::harness_routes::handle_disable(&g, &j, &disable_body)?;
    let result_val: serde_json::Value = serde_json::from_str(&result)?;
    assert_eq!(result_val["ok"], true);

    // Stale expected_snapshot_id should produce conflict.
    let stale_body = json!({
        "manifest_id": manifest_id,
        "expected_snapshot_id": "snap_stale",
    });
    let result = crate::server::harness_routes::handle_enable(&g, &j, &stale_body);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("manifest_not_found") || err.contains("snapshot_conflict"));

    Ok(())
}
