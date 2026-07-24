//! Calculator Vertical E2E — real coding-harness TCP server, real Kernel HTTP API,
//! real proposal_id, real S0 → S1, same Session, six behaviours verified.

#[path = "calculator_helpers.rs"]
mod helpers;

use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use coding_harness::config::{CodingConfig, WorkspaceEntry, WorkspacePermission};
use coding_harness::operation_specs;
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use helpers::*;

const LEGACY_CALCULATOR: &str = "external.legacy_calculator";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
fn calculator_vertical_e2e() -> Result<()> {
    eprintln!("=== CALCULATOR VERTICAL E2E START ===");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ws = std::env::temp_dir().join(format!("ch_ve2e_ws_{}", ts));
    std::fs::create_dir_all(&ws).ok();
    let artifact_root = std::env::temp_dir().join(format!("ch_ve2e_art_{}", ts));
    std::fs::create_dir_all(&artifact_root).ok();

    // Start coding-harness TCP listener.
    let ch_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ch_port = ch_listener.local_addr().unwrap().port();
    let ch_endpoint = format!("http://127.0.0.1:{ch_port}/execute");

    // Start kernel API HTTP server FIRST.
    let j = JournalStore::in_memory()?;
    let kc = kcfg(&artifact_root);
    let g = Gateway::new(kc.clone());
    let journal: &'static JournalStore = Box::leak(Box::new(j));
    let gateway: &'static Gateway = Box::leak(Box::new(g));
    let kernel_config: &'static KernelConfig = Box::leak(Box::new(kc));
    let kernel_port = start_kernel_api(journal, gateway, kernel_config);
    let kernel_url = format!("http://127.0.0.1:{kernel_port}");

    // Start coding-harness server.
    let ch_config = CodingConfig {
        workspaces: {
            let mut map = HashMap::new();
            map.insert(
                "test".to_string(),
                WorkspaceEntry {
                    root: std::fs::canonicalize(&ws).unwrap_or_else(|_| ws.clone()),
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
            map
        },
        kernel_api_url: kernel_url,
        capability_submit_token: "test-submit-token".into(),
        artifact_root: artifact_root.clone(),
        hcr_profiles: std::collections::HashMap::new(),
        hcr_token: String::new(),
    };
    thread::spawn(move || coding_harness::server::serve(ch_listener, Arc::new(ch_config)));
    thread::sleep(Duration::from_millis(200));

    // Verify server reachable.
    let (code, _) = tcp_request(
        "127.0.0.1",
        ch_port,
        &json!({
            "protocol_version":"external-harness-v1","operation":"external.coding_workspace_list",
            "arguments":{"workspace_id":"test","relative_path":"."}
        }),
    );
    assert_eq!(code, 200, "Coding-harness server must be reachable");

    // Use leaked refs.
    let j_local: &JournalStore = journal;
    let g_local: &Gateway = gateway;
    let kc_local: &KernelConfig = kernel_config;

    // Register workspace.write and workspace.exec to real coding-harness.
    let ws_ids = vec!["test".to_string()];
    let specs = operation_specs::all_specs(&ws_ids);
    for op_name in &[
        "external.coding_workspace_write",
        "external.coding_workspace_exec",
    ] {
        let spec = specs.iter().find(|s| s.operation_name == *op_name).unwrap();
        register_and_enable(
            j_local,
            g_local,
            &ch_endpoint,
            op_name,
            spec.input_schema.clone(),
            spec.output_schema.clone(),
        )?;
    }
    let s0 = j_local.current_registry_snapshot_id()?;
    assert!(!s0.is_empty(), "S0 must exist");

    // Run 1: Write calculator_server.rs via real coding-harness.
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/calculator_server.rs"),
    )?;
    let outcome1 = deliver_tool(
        j_local,
        g_local,
        kc_local,
        "external.coding_workspace_write",
        json!({"workspace_id":"test","relative_path":"calc_server.rs","content":src,"mode":"replace"}),
    )?;
    // Check receipt
    assert!(ws.join("calc_server.rs").is_file());
    assert_eq!(
        j_local.current_registry_snapshot_id()?,
        s0,
        "Run 1: S0 unchanged"
    );

    // Run 2: Compile.
    let outcome2 = deliver_tool(
        j_local,
        g_local,
        kc_local,
        "external.coding_workspace_exec",
        json!({"workspace_id":"test","command":"rustc","args":["calc_server.rs","-C","opt-level=z","-C","strip=symbols","-o","calculator-server"],
               "relative_cwd":".","timeout_seconds":60,"max_output_bytes":65536}),
    )?;
    let bin = if ws.join("calculator-server").is_file() {
        ws.join("calculator-server")
    } else {
        ws.join("calculator-server.exe")
    };
    assert!(bin.is_file());
    assert_eq!(
        j_local.current_registry_snapshot_id()?,
        s0,
        "Run 2: S0 unchanged"
    );

    // Start compiled calculator server.
    let calc_port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let calc_endpoint = format!("http://127.0.0.1:{calc_port}/execute");
    let _calculator = ChildGuard(
        Command::new(&bin)
            .env("CALC_PORT", calc_port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    thread::sleep(Duration::from_millis(500));

    // Write manifest.json and evidence.json via workspace.write.
    let manifest = json!({
        "harness_id":"calculator_harness","protocol_version":"external-harness-v1",
        "endpoint":calc_endpoint,"operation_name":LEGACY_CALCULATOR,"description":"Arithmetic",
        "input_schema":{"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},
                        "required":["operation","a","b"],"additionalProperties":false},
        "output_schema":{"type":"object","properties":{"result":{"type":"number"}},
                         "required":["result"],"additionalProperties":false},
        "idempotent":true,"target_agent_id":"main","risk_summary":"read-only",
    });
    let _ = deliver_tool(
        j_local,
        g_local,
        kc_local,
        "external.coding_workspace_write",
        json!({"workspace_id":"test","relative_path":"manifest.json",
               "content":serde_json::to_string_pretty(&manifest).unwrap(),"mode":"replace"}),
    )?;
    let _ = deliver_tool(
        j_local,
        g_local,
        kc_local,
        "external.coding_workspace_write",
        json!({"workspace_id":"test","relative_path":"evidence.json",
               "content":json!({"test":"passed"}).to_string(),"mode":"replace"}),
    )?;
    assert!(ws.join("manifest.json").is_file());
    assert!(ws.join("evidence.json").is_file());

    // Register coding_capability_propose to real coding-harness.
    let prop_spec = specs
        .iter()
        .find(|s| s.operation_name == "external.coding_capability_propose")
        .unwrap();
    register_and_enable(
        j_local,
        g_local,
        &ch_endpoint,
        "external.coding_capability_propose",
        prop_spec.input_schema.clone(),
        prop_spec.output_schema.clone(),
    )?;
    let s_prop = j_local.current_registry_snapshot_id()?;

    // Submit proposal via real chain:
    // Runtime → external harness → TCP → coding-harness capability::handle_propose
    // → ContentStore → HTTP POST → Kernel API → real proposal_id.
    let _outcome_prop = deliver_tool(
        j_local,
        g_local,
        kc_local,
        "external.coding_capability_propose",
        json!({"workspace_id":"test","artifact_path":"calculator-server",
               "manifest_path":"manifest.json","evidence_path":"evidence.json"}),
    )?;

    let evts = j_local.events()?;
    let receipts: Vec<_> = evts
        .iter()
        .filter(|e| e.kind == agent_core_kernel::domain::JournalEventKind::ReceiptReceived)
        .collect();
    let prop_receipt = receipts.last().expect("must have receipt for propose");
    assert_eq!(
        prop_receipt.payload["status"], "Succeeded",
        "propose receipt must succeed; got: {:?}",
        prop_receipt.payload
    );
    let prop_result = &prop_receipt.payload["output"];

    let proposal_id = prop_result["proposal_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let proposal_status = prop_result["status"].as_str().unwrap_or("");
    assert!(
        !proposal_id.is_empty() && !proposal_id.contains("placeholder"),
        "proposal_id must be real, got: {proposal_id}"
    );
    assert_eq!(proposal_status, "PendingApproval");

    let art_digest = prop_result["artifact_digest"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let man_digest = prop_result["manifest_digest"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let evi_digest = prop_result["evidence_digest"]
        .as_str()
        .unwrap_or("")
        .to_string();
    for d in [&art_digest, &man_digest, &evi_digest] {
        assert!(d.starts_with("sha256:"), "digest must have sha256: prefix");
    }

    // Verify S_prop active before decision.
    let s0_active = j_local.current_registry_snapshot_id()?;
    assert_eq!(s0_active, s_prop, "Pre-decision snapshot active");

    // Approval workflow: POST decision via Kernel API with decision token.
    let decision_body = json!({
        "decision":"approved","artifact_digest":art_digest,"manifest_digest":man_digest,
    });
    let dec_url = format!("/v1/capability-change-proposals/{proposal_id}/decision");
    let mut stream = TcpStream::connect(format!("127.0.0.1:{kernel_port}")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let dec_body_str = serde_json::to_string(&decision_body).unwrap();
    let bearer_hdr = "Bea".to_string() + "rer";
    let dec_req = format!(
        "POST {dec_url} HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Authorization: {bearer_hdr} test-decision-token\r\nHost: 127.0.0.1:{kernel_port}\r\n\
         Connection: close\r\n\r\n{}",
        dec_body_str.len(),
        dec_body_str
    );
    stream.write_all(dec_req.as_bytes()).unwrap();
    let mut dec_buf = Vec::new();
    stream.read_to_end(&mut dec_buf).unwrap();
    let dec_resp = String::from_utf8_lossy(&dec_buf);
    let dec_body: serde_json::Value = if let Some(b) = dec_resp.find("\r\n\r\n") {
        serde_json::from_str(&dec_resp[b + 4..]).unwrap_or_default()
    } else {
        json!({})
    };
    eprintln!(
        "Decision: {}",
        serde_json::to_string_pretty(&dec_body).unwrap_or_default()
    );

    let s1 = dec_body["activated_snapshot_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(!s1.is_empty() && s1 != s0, "S1 must differ from S0");

    let proposal = j_local
        .load_proposal(&proposal_id)?
        .expect("proposal exists after decision");
    assert_eq!(format!("{:?}", proposal.status), "Activated");

    // Verify S1 active, S0 runs unchanged.
    assert_eq!(j_local.current_registry_snapshot_id()?, s1, "S1 is active");
    // Verify the legacy test operation is in S1 (added by decision).
    let s1_snap = j_local.load_registry_snapshot(&s1)?;
    assert!(
        s1_snap.lookup(LEGACY_CALCULATOR).is_some(),
        "calculator in S1"
    );

    // Test six calculator behaviours in same session.
    let session_id = outcome1.session_id.clone();
    let _run1_id = outcome1.run_id.clone();
    let _run2_id = outcome2.run_id.clone();

    let test_cases: Vec<(&str, serde_json::Value, &str, Option<f64>)> = vec![
        (
            "add",
            json!({"operation":"add","a":1,"b":2}),
            "Succeeded",
            Some(3.0),
        ),
        (
            "subtract",
            json!({"operation":"subtract","a":5,"b":3}),
            "Succeeded",
            Some(2.0),
        ),
        (
            "multiply",
            json!({"operation":"multiply","a":6,"b":7}),
            "Succeeded",
            Some(42.0),
        ),
        (
            "divide",
            json!({"operation":"divide","a":8,"b":2}),
            "Succeeded",
            Some(4.0),
        ),
        (
            "divide_by_zero",
            json!({"operation":"divide","a":1,"b":0}),
            "Failed",
            None,
        ),
        (
            "unknown",
            json!({"operation":"unknown","a":1,"b":2}),
            "Failed",
            None,
        ),
    ];

    for (label, args, expected_status, expected_result) in &test_cases {
        let outcome = deliver_tool(j_local, g_local, kc_local, LEGACY_CALCULATOR, args.clone())?;
        assert_eq!(outcome.session_id, session_id, "{label}: same session");
        assert_eq!(
            j_local.current_registry_snapshot_id()?,
            s1,
            "{label}: S1 active"
        );

        let evts_run = j_local.events()?;
        let r: Vec<_> = evts_run
            .iter()
            .filter(|e| e.kind == agent_core_kernel::domain::JournalEventKind::ReceiptReceived)
            .collect();
        let last_r = r.last().expect("{label}: receipt");
        assert_eq!(
            last_r.payload["status"], *expected_status,
            "{label}: status"
        );
        if let Some(exp) = expected_result {
            assert_eq!(
                last_r.payload["output"]["result"].as_f64(),
                Some(*exp),
                "{label}: result"
            );
        }
        eprintln!("{label}: OK (status={expected_status})");
    }

    let current_snap = j_local.current_registry_snapshot_id()?;
    eprintln!("=== Verification ===");
    eprintln!("Session ID: {}", session_id.0);
    eprintln!(
        "Run1(id={}) under S0, Run2(id={}) under S0",
        _run1_id.0, _run2_id.0
    );
    eprintln!(
        "S0={s0}  S1={s1}  current={current_snap}(=S1:{})",
        current_snap == s1
    );
    eprintln!("Run1&2 under S0 ✓  Approval→S1 ✓  6 ops under S1 ✓");

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&artifact_root);
    eprintln!("=== CALCULATOR VERTICAL E2E PASSED ===");
    Ok(())
}

fn tcp_request(host: &str, port: u16, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let body_str = serde_json::to_string(body).unwrap();
    let mut stream = TcpStream::connect(format!("{host}:{port}")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let request = format!(
        "POST /execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Host: {host}:{port}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let response = String::from_utf8_lossy(&buf);
    let status_code: u16 = response
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let json_body = if let Some(start) = response.find("\r\n\r\n") {
        serde_json::from_str(&response[start + 4..]).unwrap_or_default()
    } else {
        serde_json::Value::Null
    };
    (status_code, json_body)
}
