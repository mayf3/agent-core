//! Legacy capability activation E2E: proposal/approval + Capability Host execution.
//!
//! This deliberately exercises the pre-HCR approval path with a non-North-Star
//! operation. `external.calculator` is reserved for the trusted HCR activation
//! flow and is covered by the dedicated PR3B North Star test.
//!
//! Flow:
//!   S0 (builtin ops only)
//!     → write calculator source via workspace.write
//!     → compile via workspace.exec (rustc)
//!     → submit Proposal via coding_capability_propose → Kernel API
//!     → PendingApproval
//!     → approve via Kernel API decision endpoint
//!     → S1 (external.legacy_calculator added)
//!     → S0 Run cannot see calculator
//!     → new Run calls external.legacy_calculator → Capability Host → Calculator → 42

#[path = "calculator_helpers.rs"]
mod helpers;

use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use coding_harness::config::{CodingConfig, WorkspaceEntry, WorkspacePermission};
use coding_harness::operation_specs;
use serde_json::json;
use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use helpers::*;

/// Calculator source code (stdlib-only, compiled by rustc in workspace).
/// File at tests/fixtures/calculator_process.rs
const CALCULATOR_SOURCE: &str = include_str!("fixtures/calculator_process.rs");

/// Start Capability Host on a random port.
fn start_capability_host(artifact_root: &PathBuf) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = Arc::new(AtomicBool::new(false));
    let s = shutdown.clone();
    let root = artifact_root.clone();
    thread::spawn(move || {
        let config = capability_host::config::CapabilityHostConfig {
            listen_addr: format!("127.0.0.1:{port}"),
            artifact_root: root,
            exec_timeout: Duration::from_secs(30),
            max_stdout_bytes: 65536,
            max_stderr_bytes: 65536,
            control_token: "legacy-test-control-token".to_string(),
            execution_token: "legacy-test-execution-token".to_string(),
        };
        for stream in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(mut stream) = stream {
                let response = handle_ch_request(&mut stream, &config);
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
    let mut rl = String::new();
    if reader.read_line(&mut rl).is_err() {
        return ch_500();
    }
    let parts: Vec<&str> = rl.split_whitespace().collect();
    if parts.len() < 2 {
        return ch_500();
    }
    let path = parts[1];
    let mut cl: usize = 0;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        if h.to_ascii_lowercase().starts_with("content-length:") {
            cl = h
                .split(':')
                .nth(1)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
        }
    }
    let mut body = String::new();
    if cl > 0 {
        let mut buf = vec![0u8; cl];
        reader.read_exact(&mut buf).ok();
        body = String::from_utf8(buf).unwrap_or_default();
    }
    if path == "/health" {
        return ch_200(r#"{"status":"ok"}"#);
    }
    let bj: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return ch_err("malformed_request"),
    };
    let req = match capability_host::protocol::parse_harness_request(&bj) {
        Ok(r) => r,
        Err(m) => return ch_err(&m),
    };
    let ap = match capability_host::artifact::resolve_artifact(
        &config.artifact_root,
        &req.artifact_digest,
    ) {
        Ok(p) => p,
        Err(_) => return ch_err("artifact_not_found"),
    };
    let sj = serde_json::to_string(&capability_host::protocol::build_process_request(&req))
        .unwrap_or_default();
    match capability_host::process::run_artifact(
        &ap,
        &sj,
        config.exec_timeout,
        config.max_stdout_bytes,
        config.max_stderr_bytes,
    ) {
        Ok(out) => {
            if out.exit_code != Some(0) {
                return ch_err("artifact_failed");
            }
            let (ok, rb) = capability_host::protocol::map_process_response(&out.stdout);
            if ok {
                ch_200(&serde_json::to_string(&rb).unwrap_or_default())
            } else {
                ch_err(
                    rb.get("error_code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("artifact_failed"),
                )
            }
        }
        Err(capability_host::process::ProcessError::Timeout) => ch_err("artifact_timeout"),
        Err(capability_host::process::ProcessError::IoError(m)) => {
            ch_err(&format!("artifact_exec_error:{m}"))
        }
    }
}
fn ch_200(b: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        b.len(),
        b
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
fn legacy_capability_activation_e2e_capability_host() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ws_root = std::env::temp_dir().join(format!("legacy_capability_e2e_ws_{ts}"));
    std::fs::create_dir_all(&ws_root).unwrap();
    let artifact_root = std::env::temp_dir().join(format!("legacy_capability_e2e_art_{ts}"));
    std::fs::create_dir_all(&artifact_root).unwrap();

    // 1. Start coding-harness TCP server.
    let ch_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ch_port = ch_listener.local_addr().unwrap().port();
    let ch_endpoint = format!("http://127.0.0.1:{ch_port}/execute");

    // 2. Start Kernel API server.
    let j = JournalStore::in_memory().unwrap();
    let kc = kcfg(&artifact_root);
    let g = Gateway::new(kc.clone());
    let journal: &'static JournalStore = Box::leak(Box::new(j));
    let gateway: &'static Gateway = Box::leak(Box::new(g));
    let kernel_config: &'static KernelConfig = Box::leak(Box::new(kc));
    let kernel_port = start_kernel_api(journal, gateway, kernel_config);
    let kernel_url = format!("http://127.0.0.1:{kernel_port}");

    // 3. Start Capability Host.
    let (ch_host_port, _ch_shutdown) = start_capability_host(&artifact_root);
    let ch_host_endpoint = format!("http://127.0.0.1:{ch_host_port}/execute");

    // 4. Create workspace and start coding-harness.
    let ws_ids = vec!["test".to_string()];
    let mut workmap = HashMap::new();
    workmap.insert(
        "test".to_string(),
        WorkspaceEntry {
            root: ws_root.clone(),
            perm: WorkspacePermission {
                read: true,
                write: true,
                exec: true,
                opencode: true,
                network: true,
                shell: false,
            },
        },
    );
    let ch_config = Arc::new(CodingConfig {
        workspaces: workmap,
        kernel_api_url: kernel_url,
        capability_submit_token: "test-submit-token".to_string(),
        artifact_root: artifact_root.clone(),
        hcr_profiles: std::collections::HashMap::new(),
        hcr_token: String::new(),
    });
    thread::spawn(move || {
        coding_harness::server::serve(ch_listener, ch_config);
    });
    thread::sleep(Duration::from_millis(200));

    // 5. Register workspace ops.
    let specs = operation_specs::all_specs(&ws_ids);
    for spec in &specs {
        if spec.operation_name == "external.coding_capability_propose" {
            continue;
        }
        register_and_enable(
            journal,
            gateway,
            &ch_endpoint,
            spec.operation_name,
            spec.input_schema.clone(),
            spec.output_schema.clone(),
        )
        .unwrap();
    }

    // 6. Capture S0.
    let s0_id = journal.current_registry_snapshot_id().unwrap();
    eprintln!("S0: {s0_id}");

    // 7. Write calculator source via workspace.write.
    deliver_tool(
        journal,
        gateway,
        kernel_config,
        "external.coding_workspace_write",
        json!({
            "workspace_id": "test", "relative_path": "calc.rs",
            "content": CALCULATOR_SOURCE, "mode": "replace",
        }),
    )
    .unwrap();

    // 8. Compile via workspace.exec (rustc).
    let _compile_out = deliver_tool(
        journal,
        gateway,
        kernel_config,
        "external.coding_workspace_exec",
        json!({
            "workspace_id": "test", "command": "rustc",
            "args": ["calc.rs", "-o", "calculator-artifact"],
            "relative_cwd": ".", "timeout_seconds": 60, "max_output_bytes": 65536,
        }),
    )
    .unwrap();
    let binary_path = ws_root.join("calculator-artifact");
    assert!(binary_path.exists(), "compiled binary must exist");

    // 9. Binary exists, compiled by workspace.exec above.
    eprintln!("calculator binary compiled successfully");

    // 10. Read binary and store in ContentStore.
    let calc_bytes = std::fs::read(&binary_path).unwrap();
    let calc_digest = Sha256Digest::compute(&calc_bytes);
    let calc_digest_str = calc_digest.as_str().to_string();
    ContentStore::new(artifact_root.clone())
        .store(&calc_bytes)
        .unwrap();
    eprintln!("artifact digest: {calc_digest_str}");

    // 11. Register coding_capability_propose.
    let prop_spec = specs
        .iter()
        .find(|s| s.operation_name == "external.coding_capability_propose")
        .unwrap();
    register_and_enable(
        journal,
        gateway,
        &ch_endpoint,
        "external.coding_capability_propose",
        prop_spec.input_schema.clone(),
        prop_spec.output_schema.clone(),
    )
    .unwrap();

    // 12. Create manifest with endpoint -> Capability Host.
    let manifest = agent_core_kernel::harness::manifest::HarnessManifest {
        manifest_id: String::new(),
        harness_id: "legacy-capability-e2e-calc".into(),
        artifact_digest: calc_digest_str.clone(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ch_host_endpoint.clone(),
        operation_name: "external.legacy_calculator".into(),
        description: "Legacy calculator capability activation E2E".into(),
        input_schema: json!({
            "type":"object","properties":{
                "operation":{"type":"string","enum":["add","subtract","multiply","divide"]},
                "a":{"type":"number"},"b":{"type":"number"}
            },"required":["operation","a","b"],"additionalProperties":false
        }),
        output_schema: json!({"type":"number"}),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    let _manifest_id = manifest.compute_manifest_id().unwrap();
    let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
    let evidence_json = r#"{"test":"passed"}"#;

    // Write manifest.json and evidence.json.
    deliver_tool(journal, gateway, kernel_config, "external.coding_workspace_write", json!({
        "workspace_id":"test","relative_path":"manifest.json","content":&manifest_json,"mode":"replace",
    })).unwrap();
    deliver_tool(journal, gateway, kernel_config, "external.coding_workspace_write", json!({
        "workspace_id":"test","relative_path":"evidence.json","content":evidence_json,"mode":"replace",
    })).unwrap();

    // 13. Submit proposal via coding_capability_propose.
    let propose_out = deliver_tool(
        journal,
        gateway,
        kernel_config,
        "external.coding_capability_propose",
        json!({
            "workspace_id":"test","artifact_path":"calculator-artifact",
            "manifest_path":"manifest.json","evidence_path":"evidence.json",
        }),
    )
    .unwrap();
    eprintln!("propose: {}", propose_out.output);

    // Parse proposal response.
    let events = journal.events().unwrap();
    let mut proposal_id = String::new();
    let mut proposal_status = String::new();
    let mut prop_artifact_digest = String::new();
    let mut prop_manifest_digest = String::new();
    for ev in &events {
        if ev.kind == JournalEventKind::ReceiptReceived {
            if let Some(output) = ev.payload.get("output") {
                if let Some(pid) = output.get("proposal_id").and_then(|v| v.as_str()) {
                    proposal_id = pid.to_string();
                }
                if let Some(ps) = output.get("status").and_then(|v| v.as_str()) {
                    proposal_status = ps.to_string();
                }
                if let Some(ad) = output.get("artifact_digest").and_then(|v| v.as_str()) {
                    prop_artifact_digest = ad.to_string();
                }
                if let Some(md) = output.get("manifest_digest").and_then(|v| v.as_str()) {
                    prop_manifest_digest = md.to_string();
                }
            }
        }
    }
    assert!(!proposal_id.is_empty(), "must have real proposal_id");
    assert_eq!(
        proposal_status, "PendingApproval",
        "status must be PendingApproval"
    );
    assert!(
        prop_artifact_digest.starts_with("sha256:"),
        "artifact digest prefix"
    );
    assert_eq!(
        prop_artifact_digest, calc_digest_str,
        "proposal digest must match"
    );
    eprintln!("Proposal: {proposal_id} status={proposal_status}");

    // 14. Verify coding config has no decision token.
    // (test-submit-token only, no decision token configured)

    // 15. Approve via Kernel API.
    let decision_body = json!({"decision":"approved","artifact_digest":prop_artifact_digest,"manifest_digest":prop_manifest_digest});
    let mut stream = TcpStream::connect(format!("127.0.0.1:{kernel_port}")).unwrap();
    let decision_path = format!("/v1/capability-change-proposals/{proposal_id}/decision");
    let req_body = serde_json::to_string(&decision_body).unwrap();
    stream.write_all(format!(
        "POST {decision_path} HTTP/1.1\r\nHost: 127.0.0.1:{kernel_port}\r\nAuthorization: Bearer test-decision-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        req_body.len(), req_body
    ).as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let sc: u16 = response
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert_eq!(sc, 200, "decision 200: {response}");

    // 16. Verify S0→S1.
    let s1_id = journal.current_registry_snapshot_id().unwrap();
    assert_ne!(s1_id, s0_id, "S1 must differ from S0");
    eprintln!("S1: {s1_id}");
    let s1_snap = journal.load_registry_snapshot(&s1_id).unwrap();
    let calc_spec = s1_snap
        .lookup("external.legacy_calculator")
        .expect("legacy calculator in S1");
    assert_eq!(
        calc_spec.binding_kind,
        agent_core_kernel::registry::snapshot::BindingKind::External
    );

    // 17. S0 Run cannot see the legacy calculator.
    let s0_snap = journal.load_registry_snapshot(&s0_id).unwrap();
    assert!(
        s0_snap.lookup("external.legacy_calculator").is_none(),
        "S0 must not have legacy calculator"
    );

    // 18. S1 Run dispatches the legacy calculator to Capability Host → 42.
    let calc_outcome = deliver_tool(
        journal,
        gateway,
        kernel_config,
        "external.legacy_calculator",
        json!({"operation":"multiply","a":6,"b":7}),
    )
    .unwrap();
    eprintln!("calculator: {}", calc_outcome.output);

    let events2 = journal.events().unwrap();
    let receipts: Vec<&JournalEvent> = events2
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .filter(|e| e.payload.get("output").and_then(|o| o.as_i64()) == Some(42))
        .collect();
    assert_eq!(receipts.len(), 1, "one receipt with 42");
    assert_eq!(
        receipts[0].payload.get("status").and_then(|v| v.as_str()),
        Some("Succeeded")
    );
    assert!(journal.verify_hash_chain().unwrap(), "hash chain valid");
    eprintln!("=== LEGACY CAPABILITY PASS: multiply(6,7)=42 via legacy path ===");
}
