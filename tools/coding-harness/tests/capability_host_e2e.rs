//! Full E2E: Kernel → Capability Host → Calculator Artifact → Receipt.
//!
//! Proves the complete artifact execution pipeline:
//! 1. Store calculator artifact in ContentStore
//! 2. Create manifest pointing to Capability Host
//! 3. Register and enable (simulating proposal + approval)
//! 4. S0 → S1 snapshot transition
//! 5. Runtime::deliver() dispatches external.calculator(multiply,6,7)
//! 6. Kernel dispatches to Capability Host
//! 7. Capability Host loads & executes calculator artifact
//! 8. Receipt = Succeeded, result = 42
//! 9. Journal hash chain valid

#[path = "calculator_helpers.rs"]
mod helpers;

use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use agent_core_kernel::harness::manifest::HarnessManifest;
use agent_core_kernel::journal::JournalStore;
use serde_json::json;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use helpers::*;

/// Locate the calculator-artifact binary built by this crate.
fn calculator_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_calculator-artifact"))
}

/// Start Capability Host on a random port. Returns (port, shutdown_flag).
fn start_capability_host(artifact_root: &PathBuf) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = Arc::new(AtomicBool::new(false));
    let s = shutdown.clone();
    let root = artifact_root.clone();
    thread::spawn(move || {
        let host_cfg = capability_host::config::CapabilityHostConfig {
            listen_addr: format!("127.0.0.1:{port}"),
            artifact_root: root,
            exec_timeout: Duration::from_secs(30),
            max_stdout_bytes: 65536,
            max_stderr_bytes: 65536,
        };
        for stream in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(mut stream) = stream {
                let response = handle_ch_request(&mut stream, &host_cfg);
                let _ = stream.write_all(response.as_bytes());
            }
        }
    });
    (port, shutdown)
}

fn handle_ch_request(
    stream: &mut TcpStream,
    config: &capability_host::config::CapabilityHostConfig,
) -> String {
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return ch_500();
    }
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return ch_500();
    }
    let path = parts[1];

    let mut content_length: usize = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if header.to_ascii_lowercase().starts_with("content-length:") {
            content_length = header
                .split(':')
                .nth(1)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
        }
    }

    let mut body = String::new();
    if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).is_ok() {
            body = String::from_utf8(buf).unwrap_or_default();
        }
    }

    if path == "/health" {
        return ch_200(r#"{"status":"ok"}"#);
    }

    let body_json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return ch_err("malformed_request"),
    };
    let req = match capability_host::protocol::parse_harness_request(&body_json) {
        Ok(r) => r,
        Err(msg) => return ch_err(&msg),
    };

    let artifact_path = match capability_host::artifact::resolve_artifact(
        &config.artifact_root,
        &req.artifact_digest,
    ) {
        Ok(p) => p,
        Err(_) => return ch_err("artifact_not_found"),
    };

    let process_req = capability_host::protocol::build_process_request(&req);
    let stdin_json = serde_json::to_string(&process_req).unwrap_or_default();
    let result = capability_host::process::run_artifact(
        &artifact_path,
        &stdin_json,
        config.exec_timeout,
        config.max_stdout_bytes,
        config.max_stderr_bytes,
    );

    match result {
        Ok(output) => {
            if output.exit_code != Some(0) {
                return ch_err("artifact_failed");
            }
            let (ok, resp_body) = capability_host::protocol::map_process_response(&output.stdout);
            if ok {
                ch_200(&serde_json::to_string(&resp_body).unwrap_or_default())
            } else {
                let ec = resp_body
                    .get("error_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("artifact_failed");
                ch_err(ec)
            }
        }
        Err(capability_host::process::ProcessError::Timeout) => ch_err("artifact_timeout"),
        Err(capability_host::process::ProcessError::IoError(msg)) => {
            ch_err(&format!("artifact_exec_error:{msg}"))
        }
    }
}

fn ch_200(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}
fn ch_500() -> String {
    "HTTP/1.1 500\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
}
fn ch_err(ec: &str) -> String {
    ch_200(&format!(
        r#"{{"protocol_version":"external-harness-v1","ok":false,"error_code":"{ec}"}}"#
    ))
}

#[test]
fn e2e_capability_host_calculator_returns_42() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let artifact_root = std::env::temp_dir().join(format!("ch_e2e_art_{ts}"));
    std::fs::create_dir_all(&artifact_root).unwrap();

    // 1. Store the calculator artifact in ContentStore.
    let calc_path = calculator_binary();
    assert!(
        calc_path.exists(),
        "calculator binary not found at {:?}",
        calc_path
    );
    let calc_bytes = std::fs::read(&calc_path).unwrap();
    let calc_digest = Sha256Digest::compute(&calc_bytes);
    {
        let store = ContentStore::new(artifact_root.clone());
        store.store(&calc_bytes).unwrap();
    }
    eprintln!("calculator digest: {}", calc_digest.as_str());

    // 2. Start Capability Host on random port.
    let (ch_port, _ch_shutdown) = start_capability_host(&artifact_root);
    let ch_endpoint = format!("http://127.0.0.1:{ch_port}/execute");
    thread::sleep(Duration::from_millis(200));

    // 3. Set up kernel journal with the calculator manifest registered and enabled.
    let config = kcfg(&artifact_root);
    let j = JournalStore::in_memory().unwrap();
    let g = Gateway::new(config.clone());

    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "ch-e2e".into(),
        artifact_digest: calc_digest.as_str().to_string(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ch_endpoint.clone(),
        operation_name: "external.calculator".into(),
        description: "Calculator e2e".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string", "enum": ["add","subtract","multiply","divide"]},
                "a": {"type": "number"},
                "b": {"type": "number"}
            },
            "required": ["operation", "a", "b"],
            "additionalProperties": false
        }),
        output_schema: json!({"type": "number"}),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    let manifest_id = manifest.compute_manifest_id().unwrap();
    manifest.manifest_id = manifest_id.clone();
    j.register_harness_manifest(&manifest).unwrap();
    j.enable_harness(
        &g.approve_harness_change(HarnessChangeIntent {
            action: HarnessChangeAction::Enable,
            manifest_id: manifest_id.clone(),
            expected_snapshot_id: j.current_registry_snapshot_id().unwrap(),
            requested_by: "ipc_operator".into(),
        })
        .unwrap(),
    )
    .unwrap();

    // 4. Verify snapshot transition and operation is present.
    let snapshot_id = j.current_registry_snapshot_id().unwrap();
    let snap = j.load_registry_snapshot(&snapshot_id).unwrap();
    let calc_spec = snap
        .lookup("external.calculator")
        .expect("external.calculator must be in active snapshot");
    assert_eq!(
        calc_spec.binding_kind,
        agent_core_kernel::registry::snapshot::BindingKind::External,
        "calculator should be external binding"
    );
    assert_eq!(
        calc_spec.binding_key, manifest_id,
        "binding_key = manifest_id"
    );

    // 5. Dispatch a tool call for external.calculator(multiply,6,7) through
    //    the full Kernel Runtime::deliver pipeline.
    let outcome = deliver_tool(
        &j,
        &g,
        &config,
        "external.calculator",
        json!({"operation": "multiply", "a": 6, "b": 7}),
    )
    .expect("deliver_tool should succeed");

    // 6. The outcome is the Run's reply text. The receipt is in the journal.
    let events = j.events().unwrap();
    let receipts: Vec<_> = events
        .iter()
        .filter(|e| e.kind == agent_core_kernel::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(receipts.len(), 1, "exactly one ReceiptReceived event");

    let receipt = receipts[0];
    let status = receipt
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let output = receipt.payload.get("output").unwrap();
    eprintln!("Receipt status: {status}");
    eprintln!("Receipt output: {output}");

    assert_eq!(
        status, "Succeeded",
        "Receipt should be Succeeded, got {status}: {output}"
    );

    // Verify result is 42.
    let val = output
        .as_i64()
        .or_else(|| output.as_f64().map(|f| f as i64))
        .unwrap_or(-1);
    assert_eq!(val, 42, "multiply(6,7) should be 42, got {output}");

    // 7. Verify journal hash chain.
    assert!(
        j.verify_hash_chain().unwrap(),
        "journal hash chain must be valid"
    );

    // 8. Verify outcome is non-blank.
    assert!(!outcome.output.is_empty(), "outcome should not be empty");

    eprintln!("=== E2E PASS: multiply(6,7) = 42 via Capability Host ===");
}
