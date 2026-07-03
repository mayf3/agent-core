//! Coding Harness E2E tests — develop calculator, propose, approve, verify 42.

mod helpers;

use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;

use helpers::*;

fn cfg() -> crate::config::KernelConfig {
    crate::config::KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: crate::domain::AgentId("main".to_string()),
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
        extra_allowed_operations: vec![
            "system.status".to_string(),
            "external.coding_workspace_list".to_string(),
            "external.coding_workspace_read".to_string(),
            "external.coding_workspace_write".to_string(),
            "external.coding_workspace_exec".to_string(),
            "external.coding_task_submit".to_string(),
            "external.coding_task_status".to_string(),
            "external.coding_capability_propose".to_string(),
            "external.calculator".to_string(),
        ],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 30_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ch_e2e_{}", std::process::id())),
        capability_submit_token: None,
        capability_decision_token: None,
    }
}

#[test]
fn coding_harness_workspace_ops_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ch_ws_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _sd, _port) = start_mock_harness(dir.clone())?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(cfg());

    for name in &[
        "external.coding_workspace_list",
        "external.coding_workspace_read",
        "external.coding_workspace_write",
        "external.coding_workspace_exec",
    ] {
        let mid = register_manifest(
            &j,
            &ep,
            name,
            json!({"type":"object"}),
            json!({"type":"object"}),
        )?;
        enable_op(&j, &g, &mid)?;
    }

    let llm = SingleToolLlm::new(
        "external.coding_workspace_write",
        json!({"workspace_id":"test","relative_path":"calc.rs","content":"fn add(a:i32,b:i32)->i32{a+b}","mode":"replace"}),
    );
    let rt = super::Runtime::new(cfg(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("write?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    assert!(dir.join("calc.rs").is_file());

    let j2 = JournalStore::in_memory()?;
    let mid = register_manifest(
        &j2,
        &ep,
        "external.coding_workspace_read",
        json!({"type":"object"}),
        json!({"type":"object"}),
    )?;
    enable_op(&j2, &g, &mid)?;
    let llm2 = SingleToolLlm::new(
        "external.coding_workspace_read",
        json!({"workspace_id":"test","relative_path":"calc.rs"}),
    );
    let rt2 = super::Runtime::new(cfg(), llm2);
    let ev2 = g.validate_ingress(&j2, g.cli_ingress("read?".into())?)?;
    let o2 = rt2.deliver(&j2, &g, ev2)?;
    assert!(!o2.output.trim().is_empty());
    let ev2 = j2.events()?;
    let r2: Vec<_> = ev2
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r2[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn calculator_development_to_42_e2e() -> Result<()> {
    let ws_root = std::env::temp_dir().join(format!("ch_calc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws_root);
    std::fs::create_dir_all(&ws_root).unwrap();

    let (ch_ep, _ch_sd, _ch_port) = start_mock_harness(ws_root.clone())?;
    let (calc_ep, _calc_sd, _calc_port) = start_calculator_harness()?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(cfg());

    let s0 = j.current_registry_snapshot_id()?;
    assert!(j
        .load_registry_snapshot(&s0)?
        .lookup("external.calculator")
        .is_none());

    for name in &[
        "external.coding_workspace_write",
        "external.coding_workspace_exec",
    ] {
        let mid = register_manifest(
            &j,
            &ch_ep,
            name,
            json!({"type":"object"}),
            json!({"type":"object"}),
        )?;
        enable_op(&j, &g, &mid)?;
    }

    let src = r#"fn main() { let a:f64=std::env::args().nth(2).unwrap_or("0").parse().unwrap_or(0.0); let b:f64=std::env::args().nth(3).unwrap_or("0").parse().unwrap_or(0.0); let r=match std::env::args().nth(1).unwrap_or("").as_str(){ "add"=>a+b,"sub"=>a-b,"mul"=>a*b,"div"=>if b==0.0{eprintln!("div_by_zero");std::process::exit(1)}else{a/b},_=>{eprintln!("unsup");std::process::exit(1)}}; println!("{}",r); }"#;
    let llm = SingleToolLlm::new(
        "external.coding_workspace_write",
        json!({"workspace_id":"test","relative_path":"calc.rs","content":src,"mode":"replace"}),
    );
    let rt = super::Runtime::new(cfg(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("write?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r[0].payload["status"], "Succeeded");
    assert!(ws_root.join("calc.rs").is_file());

    // Build
    let j2 = JournalStore::in_memory()?;
    let mid = register_manifest(
        &j2,
        &ch_ep,
        "external.coding_workspace_exec",
        json!({"type":"object"}),
        json!({"type":"object"}),
    )?;
    enable_op(&j2, &g, &mid)?;
    let llm2 = SingleToolLlm::new(
        "external.coding_workspace_exec",
        json!({"workspace_id":"test","program":"rustc","args":["calc.rs","-o","calc"],"relative_cwd":".","timeout_seconds":60,"max_output_bytes":65536}),
    );
    let rt2 = super::Runtime::new(cfg(), llm2);
    let ev2 = g.validate_ingress(&j2, g.cli_ingress("build?".into())?)?;
    let o2 = rt2.deliver(&j2, &g, ev2)?;
    assert!(!o2.output.trim().is_empty());

    // Test multiply
    let j3 = JournalStore::in_memory()?;
    let mid3 = register_manifest(
        &j3,
        &ch_ep,
        "external.coding_workspace_exec",
        json!({"type":"object"}),
        json!({"type":"object"}),
    )?;
    enable_op(&j3, &g, &mid3)?;
    let llm3 = SingleToolLlm::new(
        "external.coding_workspace_exec",
        json!({"workspace_id":"test","program":"./calc","args":["mul","6","7"],"relative_cwd":".","timeout_seconds":30,"max_output_bytes":1024}),
    );
    let rt3 = super::Runtime::new(cfg(), llm3);
    let ev3 = g.validate_ingress(&j3, g.cli_ingress("6*7?".into())?)?;
    let o3 = rt3.deliver(&j3, &g, ev3)?;
    assert!(!o3.output.trim().is_empty());

    // Register calculator harness
    let calc_mid = register_manifest(
        &j,
        &calc_ep,
        "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"]}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"]}),
    )?;
    enable_op(&j, &g, &calc_mid)?;

    // S1
    let s1 = j.current_registry_snapshot_id()?;
    assert_ne!(s0, s1);
    assert!(j
        .load_registry_snapshot(&s1)?
        .lookup("external.calculator")
        .is_some());

    // Call multiply(6,7) → 42
    let j4 = JournalStore::in_memory()?;
    let cmid = register_manifest(
        &j4,
        &calc_ep,
        "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"]}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"]}),
    )?;
    enable_op(&j4, &g, &cmid)?;

    let llm4 = SingleToolLlm::new(
        "external.calculator",
        json!({"operation":"multiply","a":6,"b":7}),
    );
    let rt4 = super::Runtime::new(cfg(), llm4);
    let ev4 = g.validate_ingress(&j4, g.cli_ingress("6*7?".into())?)?;
    let o4 = rt4.deliver(&j4, &g, ev4)?;
    assert!(!o4.output.trim().is_empty());

    let ev4 = j4.events()?;
    let r4: Vec<_> = ev4
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r4.len(), 1);
    assert_eq!(r4[0].payload["status"], "Succeeded");
    let out = serde_json::to_string(&r4[0].payload["output"]).unwrap_or_default();
    assert!(
        out.contains("42") || out.contains("result"),
        "Receipt must contain 42; got: {out}"
    );

    let _ = std::fs::remove_dir_all(&ws_root);
    Ok(())
}
