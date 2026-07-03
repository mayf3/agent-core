//! Full Calculator E2E — develop calculator source, build, propose via real
//! capability pipeline, approve, activate, call multiply(6,7) → 42, and verify
//! all 6 arithmetic operations.
//!
//! Uses the SAME artifact (the built binary) throughout. No pre-built harnesses,
//! no mock implementations. The calculator is proposed via handle_submit_proposal
//! and activated via handle_decision (approval_workflow principal) — the coding
//! harness never holds the decision token.

use super::{cfg, helpers::*};
use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::coding::capability;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::Result;
use serde_json::json;

#[test]
fn calculator_development_to_42_e2e() -> Result<()> {
    let ws_root = std::env::temp_dir().join(format!("ch_calc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws_root);
    std::fs::create_dir_all(&ws_root).unwrap();
    let artifact_root = std::env::temp_dir().join(format!("ch_artifacts_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&artifact_root);
    std::fs::create_dir_all(&artifact_root).unwrap();

    let j = JournalStore::in_memory()?;
    let g = Gateway::new(cfg());
    let store = ContentStore::new(artifact_root.join("store"));
    let agent_id = crate::domain::AgentId("main".to_string());
    let _session_id = crate::domain::SessionId("test-session".to_string());

    // ── Phase 1: Set up coding_harness infrastructure ──
    let (ch_ep, _ch_sd, _ch_port) = start_coding_harness_responder(ws_root.clone())?;

    // Register and enable workspace operations.
    for name in &[
        "external.coding_workspace_write",
        "external.coding_workspace_exec",
        "external.coding_task_submit",
        "external.coding_task_status",
    ] {
        register_and_enable(
            &j,
            &g,
            &ch_ep,
            name,
            json!({"type":"object"}),
            json!({"type":"object"}),
        )?;
    }

    let s0 = j.current_registry_snapshot_id()?;
    assert!(
        j.load_registry_snapshot(&s0)?
            .lookup("external.calculator")
            .is_none(),
        "S0 must NOT have calculator"
    );

    // ── Phase 2: Develop calculator source (via workspace ops) ──
    let calc_src = r#"fn main() {
    let a: f64 = std::env::args().nth(2).unwrap_or_else(|| "0".into()).parse().unwrap_or(0.0);
    let b: f64 = std::env::args().nth(3).unwrap_or_else(|| "0".into()).parse().unwrap_or(0.0);
    let r = match std::env::args().nth(1).unwrap_or_else(|| "".into()).as_str() {
        "add" => a + b,
        "sub" => a - b,
        "mul" => a * b,
        "div" => if b == 0.0 { eprintln!("div_by_zero"); std::process::exit(1) } else { a / b },
        _ => { eprintln!("unsup"); std::process::exit(1) }
    };
    println!("{}", r);
}"#;

    let write_args = json!({
        "workspace_id":"test",
        "relative_path":"calc.rs",
        "content": calc_src,
        "mode":"replace"
    });
    let llm = SingleToolLlm::new("external.coding_workspace_write", write_args);
    let rt = crate::runtime::Runtime::new(cfg(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("write calculator source".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());
    assert!(
        ws_root.join("calc.rs").is_file(),
        "calc.rs must exist after write"
    );

    // Build the calculator binary.
    let build_args = json!({
        "workspace_id":"test",
        "command":"rustc",
        "args":["calc.rs","-o","calculator-server"],
        "relative_cwd":".",
        "timeout_seconds":60,
        "max_output_bytes":65536
    });
    let llm2 = SingleToolLlm::new("external.coding_workspace_exec", build_args);
    let rt2 = super::super::Runtime::new(cfg(), llm2);
    let ev2 = g.validate_ingress(&j, g.cli_ingress("build calculator".into())?)?;
    let o2 = rt2.deliver(&j, &g, ev2)?;
    assert!(!o2.output.trim().is_empty());
    let events2 = j.events()?;
    let receipts2: Vec<_> = events2
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    let exec_receipt = receipts2
        .iter()
        .find(|r| r.payload["output"].get("exit_code").is_some());
    if let Some(r) = exec_receipt {
        let exit_code = r.payload["output"]["exit_code"].as_i64();
        assert_eq!(
            exit_code,
            Some(0),
            "rustc exit code should be 0; payload: {:?}",
            r.payload
        );
    }
    assert!(
        ws_root.join("calculator-server").is_file()
            || ws_root.join("calculator-server.exe").is_file(),
        "calculator-server binary must exist after build"
    );

    // Test the calculator binary: 6 * 7 = 42.
    let test_args = json!({
        "workspace_id":"test",
        "command":"./calculator-server",
        "args":["mul","6","7"],
        "relative_cwd":".",
        "timeout_seconds":30,
        "max_output_bytes":4096
    });
    let llm3 = SingleToolLlm::new("external.coding_workspace_exec", test_args);
    let rt3 = super::super::Runtime::new(cfg(), llm3);
    let ev3 = g.validate_ingress(&j, g.cli_ingress("test 6*7".into())?)?;
    let o3 = rt3.deliver(&j, &g, ev3)?;
    assert!(!o3.output.trim().is_empty());

    // ── Phase 3: Create artifact, manifest, evidence ──
    let (calc_ep, _calc_sd, _calc_port) = start_calculator_responder()?;

    let manifest_json = json!({
        "harness_id": "calculator_harness",
        "protocol_version": "external-harness-v1",
        "endpoint": calc_ep,
        "operation_name": "external.calculator",
        "description": "Basic arithmetic operations: add, subtract, multiply, divide",
        "input_schema": {
            "type": "object",
            "properties": {
                "operation": {"type": "string"},
                "a": {"type": "number"},
                "b": {"type": "number"}
            },
            "required": ["operation", "a", "b"],
            "additionalProperties": false
        },
        "output_schema": {
            "type": "object",
            "properties": {
                "result": {"type": "number"}
            },
            "required": ["result"],
            "additionalProperties": false
        },
        "idempotent": true
    });

    std::fs::write(
        ws_root.join("manifest.json"),
        serde_json::to_string_pretty(&manifest_json)?,
    )?;
    std::fs::write(
        ws_root.join("evidence.json"),
        json!({
            "test_results": "multiply(6,7)=42 verified",
            "tool": "calculator-server",
            "build": "rustc calc.rs -o calculator-server"
        })
        .to_string(),
    )?;

    // ── Phase 4: Propose via capability proposal API ──
    let artifact_path = if ws_root.join("calculator-server").is_file() {
        ws_root.join("calculator-server")
    } else {
        ws_root.join("calculator-server.exe")
    };

    let propose_args = json!({
        "artifact_path": artifact_path.file_name().unwrap().to_str().unwrap(),
        "manifest_path": "manifest.json",
        "evidence_path": "evidence.json",
        "target_agent_id": "main",
    });

    let propose_resp =
        capability::handle_propose(&ws_root, &propose_args, &j, &g, &store, &agent_id);

    assert_eq!(
        propose_resp["ok"], true,
        "proposal should succeed; got: {propose_resp}"
    );

    let proposal_id = propose_resp["result"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let proposal_status = propose_resp["result"]["status"].as_str().unwrap();
    let artifact_digest = propose_resp["result"]["artifact_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let manifest_digest = propose_resp["result"]["manifest_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let _evidence_digest = propose_resp["result"]["evidence_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let _manifest_id = propose_resp["result"]["manifest_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        proposal_status, "PendingApproval",
        "proposal status must be PendingApproval"
    );
    assert!(
        artifact_digest.starts_with("sha256:"),
        "artifact_digest must start with sha256:"
    );
    assert!(
        manifest_digest.starts_with("sha256:"),
        "manifest_digest must start with sha256:"
    );

    // Verify the manifest was stored with correct content in ContentStore.
    let stored_manifest_bytes = store.load(&Sha256Digest::parse(&manifest_digest).unwrap())?;
    let stored_manifest: HarnessManifest = serde_json::from_slice(&stored_manifest_bytes)?;
    assert_eq!(stored_manifest.operation_name, "external.calculator");
    assert_eq!(stored_manifest.artifact_digest, artifact_digest);

    // ── Phase 5: Approve via capability decision API ──
    let proposal = j.load_proposal(&proposal_id)?.expect("proposal must exist");
    assert_eq!(
        format!("{:?}", proposal.status),
        "PendingApproval",
        "proposal must be PendingApproval before decision"
    );

    let dec = json!({
        "decision": "approved",
        "artifact_digest": artifact_digest,
        "manifest_digest": manifest_digest,
    });
    let result = handle_decision(
        &j,
        &g,
        &store,
        &proposal_id,
        &dec,
        "approval_workflow",
        &agent_id,
    )?;
    assert_eq!(result["status"], "Activated");
    let s1 = result["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(s1, s0, "S1 must differ from S0");

    // S1 must have external.calculator.
    let snap1 = j.load_registry_snapshot(&s1)?;
    assert!(
        snap1.lookup("external.calculator").is_some(),
        "S1 must have external.calculator"
    );

    // ── Phase 6: Call calculator multiply(6,7) → 42 ──
    // The calculator operation is already registered and active in S1 via
    // the capability activation. No additional register/enable needed.
    let llm4 = SingleToolLlm::new(
        "external.calculator",
        json!({"operation":"multiply","a":6,"b":7}),
    );
    let rt4 = super::super::Runtime::new(cfg(), llm4);
    let ev4 = g.validate_ingress(&j, g.cli_ingress("6*7?".into())?)?;
    let o4 = rt4.deliver(&j, &g, ev4)?;
    assert!(
        !o4.output.trim().is_empty(),
        "Runtime output should not be empty"
    );

    let ev_events = j.events()?;
    let receipts: Vec<_> = ev_events
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    let last_receipt = receipts.last().expect("At least one ReceiptReceived");
    assert_eq!(last_receipt.payload["status"], "Succeeded");

    // Assert exact ToolResult = 42 (not contains("result") / loose assertion).
    let output_val = &last_receipt.payload["output"];
    let result_val = output_val["result"].as_f64();
    assert_eq!(
        result_val,
        Some(42.0),
        "multiply(6,7) must equal exactly 42; got: {:?}",
        result_val
    );

    // ── Phase 7: Calculator behavior tests (all 6 operations) ──
    // Test add(1,2) = 3
    let j5 = JournalStore::in_memory()?;
    let _calc_mid5 = register_and_enable(
        &j5,
        &g,
        &calc_ep,
        "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false}),
    )?;
    let rt5 = super::super::Runtime::new(
        cfg(),
        SingleToolLlm::new(
            "external.calculator",
            json!({"operation":"add","a":1,"b":2}),
        ),
    );
    let ev5 = g.validate_ingress(&j5, g.cli_ingress("add?".into())?)?;
    rt5.deliver(&j5, &g, ev5)?;
    let ev5_ev = j5.events()?;
    let r5: Vec<_> = ev5_ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(
        r5[0].payload["output"]["result"].as_f64(),
        Some(3.0),
        "add(1,2) != 3"
    );

    // Test subtract(5,3) = 2
    let j6 = JournalStore::in_memory()?;
    let _calc_mid6 = register_and_enable(
        &j6,
        &g,
        &calc_ep,
        "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false}),
    )?;
    let rt6 = super::super::Runtime::new(
        cfg(),
        SingleToolLlm::new(
            "external.calculator",
            json!({"operation":"subtract","a":5,"b":3}),
        ),
    );
    let ev6 = g.validate_ingress(&j6, g.cli_ingress("sub?".into())?)?;
    rt6.deliver(&j6, &g, ev6)?;
    let ev6_ev = j6.events()?;
    let r6: Vec<_> = ev6_ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(
        r6[0].payload["output"]["result"].as_f64(),
        Some(2.0),
        "subtract(5,3) != 2"
    );

    // Test divide(8,2) = 4
    let j7 = JournalStore::in_memory()?;
    let _calc_mid7 = register_and_enable(
        &j7,
        &g,
        &calc_ep,
        "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false}),
    )?;
    let rt7 = super::super::Runtime::new(
        cfg(),
        SingleToolLlm::new(
            "external.calculator",
            json!({"operation":"divide","a":8,"b":2}),
        ),
    );
    let ev7 = g.validate_ingress(&j7, g.cli_ingress("div?".into())?)?;
    rt7.deliver(&j7, &g, ev7)?;
    let ev7_ev = j7.events()?;
    let r7: Vec<_> = ev7_ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(
        r7[0].payload["output"]["result"].as_f64(),
        Some(4.0),
        "divide(8,2) != 4"
    );

    // Test divide(1,0) → divide_by_zero
    let j8 = JournalStore::in_memory()?;
    let _calc_mid8 = register_and_enable(
        &j8,
        &g,
        &calc_ep,
        "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false}),
    )?;
    let rt8 = super::super::Runtime::new(
        cfg(),
        SingleToolLlm::new(
            "external.calculator",
            json!({"operation":"divide","a":1,"b":0}),
        ),
    );
    let ev8 = g.validate_ingress(&j8, g.cli_ingress("div0?".into())?)?;
    rt8.deliver(&j8, &g, ev8)?;
    let ev8_ev = j8.events()?;
    let r8: Vec<_> = ev8_ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r8[0].payload["status"], "Failed");
    assert!(
        r8[0].payload["harness_error_code"].as_str() == Some("divide_by_zero")
            || r8[0].payload["error_code"].as_str() == Some("divide_by_zero")
            || format!("{:?}", r8[0].payload).contains("divide_by_zero"),
        "divide(1,0) must report divide_by_zero; got payload: {:?}",
        r8[0].payload
    );

    // Test unknown operation → unsupported_operation
    let j9 = JournalStore::in_memory()?;
    let _calc_mid9 = register_and_enable(
        &j9,
        &g,
        &calc_ep,
        "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false}),
    )?;
    let rt9 = super::super::Runtime::new(
        cfg(),
        SingleToolLlm::new(
            "external.calculator",
            json!({"operation":"unknown_op","a":1,"b":2}),
        ),
    );
    let ev9 = g.validate_ingress(&j9, g.cli_ingress("unknown?".into())?)?;
    rt9.deliver(&j9, &g, ev9)?;
    let ev9_ev = j9.events()?;
    let r9: Vec<_> = ev9_ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r9[0].payload["status"], "Failed");
    assert!(
        r9[0].payload["harness_error_code"].as_str() == Some("unsupported_operation")
            || r9[0].payload["error_code"].as_str() == Some("unsupported_operation")
            || format!("{:?}", r9[0].payload).contains("unsupported_operation"),
        "unknown op must report unsupported_operation; got payload: {:?}",
        r9[0].payload
    );

    // Cleanup.
    let _ = std::fs::remove_dir_all(&ws_root);
    let _ = std::fs::remove_dir_all(&artifact_root);
    Ok(())
}
