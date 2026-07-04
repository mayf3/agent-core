use super::helpers::*;
use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::{handle_decision, handle_submit_proposal};
use anyhow::Result;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn kcfg() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"), data_dir: PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()), root_dir: PathBuf::from("."),
        kernel_port: 0, connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![], feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
        openai_api_key: String::new(), model: String::new(),
        fallback_openai_base_url: String::new(), fallback_openai_api_key: String::new(),
        fallback_model: String::new(), model_timeout_ms: 100,
        context_recent_messages: 6, context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false, outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec![
            "system.status".into(), "external.coding_workspace_write".into(),
            "external.coding_workspace_exec".into(), "external.coding_capability_propose".into(),
            "external.calculator".into(),
        ],
        require_write_approval: false, write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false, primary_tool_name_indexed: false,
        harness_read_timeout_ms: 15_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ch_art_{}", std::process::id())),
        capability_submit_token: Some("test-submit-token".into()),
        capability_decision_token: Some("test-decision-token".into()),
    }
}

#[test]
fn calculator_development_to_42_e2e() -> Result<()> {
    let ws_root = std::env::temp_dir().join(format!("ch_calc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws_root);
    std::fs::create_dir_all(&ws_root).unwrap();
    let art_root = std::env::temp_dir().join(format!("ch_art_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&art_root);
    std::fs::create_dir_all(&art_root).unwrap();

    let j = Arc::new(JournalStore::in_memory()?);
    let g = Arc::new(Gateway::new(kcfg()));
    let store = ContentStore::new(art_root.join("store"));
    let aid = AgentId("main".to_string());

    // Workspace TCP responder
    let wl = TcpListener::bind("127.0.0.1:0").unwrap();
    let wp = wl.local_addr().unwrap().port();
    let we = format!("http://127.0.0.1:{wp}/execute");
    let wr = Arc::new(ws_root.clone());
    thread::spawn(move || {
        for s in wl.incoming() {
            let r = wr.clone();
            thread::spawn(move || {
                let mut s = match s { Ok(s) => s, _ => return };
                let mut b = [0u8; 65536];
                if s.read(&mut b).unwrap_or(0) == 0 { return; }
                let req = String::from_utf8_lossy(&b[..]);
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                let p: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                let op = p.get("operation").and_then(|v| v.as_str()).unwrap_or("");
                let a = p.get("arguments").cloned().unwrap_or(json!({}));
                let rv = match op {
                    "external.coding_workspace_write" => {
                        let rel = a.get("relative_path").and_then(|v| v.as_str()).unwrap_or("");
                        let c = a.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let p = r.join(rel);
                        if let Some(pp) = p.parent() { let _ = std::fs::create_dir_all(pp); }
                        match std::fs::write(&p, c) {
                            Ok(_) => json!({"ok":true,"result":{"bytes_written":c.len()}}),
                            Err(e) => json!({"ok":false,"error_code":format!("{e}")}),
                        }
                    }
                    "external.coding_workspace_exec" => {
                        let cmd = a.get("command").and_then(|v| v.as_str()).unwrap_or("");
                        let ca: Vec<&str> = a.get("args").and_then(|a| a.as_array())
                            .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                        let cw = r.join(a.get("relative_cwd").and_then(|v| v.as_str()).unwrap_or("."));
                        let mut ch = std::process::Command::new(cmd);
                        ch.args(&ca).current_dir(&cw);
                        ch.env_clear();
                        if let Some(v) = std::env::var_os("PATH") { ch.env("PATH", v); }
                        ch.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
                        match ch.output() {
                            Ok(o) => json!({"ok":true,"result":{
                                "exit_code": o.status.code().unwrap_or(-1),
                                "stdout": String::from_utf8_lossy(&o.stdout).into_owned(),
                                "stderr": String::from_utf8_lossy(&o.stderr).into_owned(), "timed_out": false}}),
                            Err(e) => json!({"ok":false,"error_code":format!("spawn:{e}")}),
                        }
                    }
                    _ => json!({"ok":false,"error_code":"unknown_op"}),
                };
                let rj = json!({"protocol_version":"external-harness-v1", "result": rv["result"].clone(), "ok": rv["ok"].clone()});
                let bb = serde_json::to_string(&rj).unwrap_or_default();
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bb}", bb.len()).as_bytes());
            });
        }
    });
    thread::sleep(Duration::from_millis(100));

    for n in &["external.coding_workspace_write", "external.coding_workspace_exec"] {
        register_and_enable(&*j, &*g, &we, n, json!({"type":"object"}), json!({"type":"object"}))?;
    }

    let s0 = j.current_registry_snapshot_id()?;
    assert!(j.load_registry_snapshot(&s0)?.lookup("external.calculator").is_none());

    // Write calc source
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/coding-harness/tests/fixtures/calculator_server.rs")
    ).unwrap_or_default();
    let l = SingleToolLlm::new("external.coding_workspace_write", json!({
        "workspace_id":"test","relative_path":"calc_server.rs","content":src,"mode":"replace",
    }));
    crate::runtime::Runtime::new(kcfg(), l).deliver(&*j, &*g,
        g.validate_ingress(&*j, g.cli_ingress("w".into())?)?)?;
    eprintln!("Checking for file: {:?}", ws_root.join("calc_server.rs")); assert!(ws_root.join("calc_server.rs").is_file(), "calc_server.rs not found after write");

    // Build
    let l = SingleToolLlm::new("external.coding_workspace_exec", json!({
        "workspace_id":"test","command":"rustc","args":["calc_server.rs","-o","calculator-server"],
        "relative_cwd":".","timeout_seconds":60,"max_output_bytes":65536,
    }));
    crate::runtime::Runtime::new(kcfg(), l).deliver(&*j, &*g,
        g.validate_ingress(&*j, g.cli_ingress("b".into())?)?)?;
    let bin = if ws_root.join("calculator-server").is_file() { ws_root.join("calculator-server") }
              else { ws_root.join("calculator-server.exe") };
    assert!(bin.is_file());

    // Start calculator artifact
    let calc_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let cp = calc_listener.local_addr().unwrap().port();
    let ce = format!("http://127.0.0.1:{cp}/execute");
    let bp = bin.clone();
    thread::spawn(move || { let _ = std::process::Command::new(&bp).env("CALC_PORT", cp.to_string()).spawn(); });
    thread::sleep(Duration::from_millis(500));

    // Write manifest & evidence
    let mj = json!({
        "harness_id":"calculator_harness","protocol_version":"external-harness-v1",
        "endpoint":ce,"operation_name":"external.calculator","description":"Arithmetic",
        "input_schema":{"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false},
        "output_schema":{"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false},
        "idempotent":true,"target_agent_id":"main","risk_summary":"read-only",
    });
    std::fs::write(ws_root.join("manifest.json"), serde_json::to_string_pretty(&mj).unwrap())?;
    std::fs::write(ws_root.join("evidence.json"), json!({"test":"passed"}).to_string())?;

    // Propose TCP dispatcher
    let pl = TcpListener::bind("127.0.0.1:0").unwrap();
    let pp = pl.local_addr().unwrap().port();
    let pe = format!("http://127.0.0.1:{pp}/execute");
    register_and_enable(&*j, &*g, &pe, "external.coding_capability_propose",
        json!({"type":"object"}), json!({"type":"object"}))?;

    let pj = j.clone();
    let pg = g.clone();
    let ps = ContentStore::new(art_root.join("store"));
    let pa = aid.clone();
    let pr = ws_root.clone();

    thread::spawn(move || {
        for s in pl.incoming() {
            if let Ok(mut s) = s {
                let mut b = [0u8; 65536];
                if s.read(&mut b).unwrap_or(0) == 0 { return; }
                let req = String::from_utf8_lossy(&b[..]);
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                let p: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                let a = p.get("arguments").cloned().unwrap_or(json!({}));

                let rf = |rel: &str| -> std::io::Result<Vec<u8>> { std::fs::read(pr.join(rel)) };
                let (ad, mr, ed) = match (rf(a.get("artifact_path").and_then(|v| v.as_str()).unwrap_or("")), rf(a.get("manifest_path").and_then(|v| v.as_str()).unwrap_or("")), rf(a.get("evidence_path").and_then(|v| v.as_str()).unwrap_or(""))) {
                    (Ok(a), Ok(m), Ok(e)) => (a, m, e), _ => return,
                };

                let adg = Sha256Digest::compute(&ad);
                let mv: serde_json::Value = serde_json::from_slice(&mr).unwrap_or_default();
                let mut m = HarnessManifest {
                    manifest_id: String::new(),
                    harness_id: mv.get("harness_id").and_then(|v| v.as_str()).unwrap_or("h").to_string(),
                    artifact_digest: adg.as_str().to_string(),
                    protocol_version: "external-harness-v1".into(),
                    endpoint: mv.get("endpoint").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    operation_name: mv.get("operation_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    description: mv.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    input_schema: mv.get("input_schema").cloned().unwrap_or(json!({"type":"object"})),
                    output_schema: mv.get("output_schema").cloned().unwrap_or(json!({"type":"object"})),
                    idempotent: mv.get("idempotent").and_then(|v| v.as_bool()).unwrap_or(true),
                    created_at: chrono::Utc::now(),
                };
                m.manifest_id = m.compute_manifest_id().unwrap_or_default();
                let fm = serde_json::to_vec(&m).unwrap_or_default();
                let _mdg = Sha256Digest::compute(&fm);
                let _edg = Sha256Digest::compute(&ed);
                if let (Ok(sa), Ok(sm), Ok(se)) = (ps.store(&ad), ps.store(&fm), ps.store(&ed)) {
                    let sb = json!({
                        "target_agent_id":"main","artifact_ref":"calculator-server",
                        "artifact_digest":sa.as_str(),"manifest_ref":"manifest.json",
                        "manifest_digest":sm.as_str(),"evidence_ref":"evidence.json",
                        "evidence_digest":se.as_str(),"requested_operations":[m.operation_name],
                        "risk_summary":"read-only",
                    });
                    if let Ok(resp) = handle_submit_proposal(&*pj, &*pg, &sb, "coding_harness", &pa) {
                        let rj = json!({"protocol_version":"external-harness-v1","ok":true,"result":{
                            "proposal_id":resp.proposal_id,"status":resp.status,
                            "expected_active_snapshot_id":resp.expected_active_snapshot_id,
                            "requested_operations":resp.requested_operations,"expires_at":resp.expires_at,
                            "artifact_digest":sa.as_str(),"manifest_digest":sm.as_str(),
                            "evidence_digest":se.as_str(),"manifest_id":m.manifest_id,
                            "operation_name":m.operation_name,
                        }});
                        let bb = serde_json::to_string(&rj).unwrap_or_default();
                        let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bb}", bb.len()).as_bytes());
                    }
                }
            }
        }
    });
    thread::sleep(Duration::from_millis(100));

    // Propose via real TCP operation
    let l = SingleToolLlm::new("external.coding_capability_propose", json!({
        "workspace_id":"test","artifact_path":"calculator-server",
        "manifest_path":"manifest.json","evidence_path":"evidence.json",
    }));
    crate::runtime::Runtime::new(kcfg(), l).deliver(&*j, &*g,
        g.validate_ingress(&*j, g.cli_ingress("p".into())?)?)?;
    let ev = j.events()?;
    let rc: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    let pr = &rc.last().unwrap().payload["output"];
    let pid = pr["proposal_id"].as_str().unwrap_or("").to_string();
    let pst = pr["status"].as_str().unwrap_or("");
    assert!(!pid.is_empty());
    assert_eq!(pst, "PendingApproval");
    eprintln!("Proposal: id={pid} status={pst}");

    // Approve
    let po = j.load_proposal(&pid)?.unwrap();
    let dec = json!({"decision":"approved","artifact_digest":po.artifact_digest,"manifest_digest":po.manifest_digest});
    let res = handle_decision(&*j, &*g, &store, &pid, &dec, "approval_workflow", &aid)?;
    assert_eq!(res["status"], "Activated");
    let s1 = res["activated_snapshot_id"].as_str().unwrap().to_string();
    assert_ne!(s1, s0);
    assert!(j.load_registry_snapshot(&s1)?.lookup("external.calculator").is_some());

    // Run 2: multiply(6,7) → 42
    register_and_enable(&*j, &*g, &ce, "external.calculator",
        json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false}),
        json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false}))?;

    let l = SingleToolLlm::new("external.calculator", json!({"operation":"multiply","a":6,"b":7}));
    crate::runtime::Runtime::new(kcfg(), l).deliver(&*j, &*g,
        g.validate_ingress(&*j, g.cli_ingress("m".into())?)?)?;
    let ev = j.events()?;
    let rc: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    let lr = rc.last().unwrap();
    assert_eq!(lr.payload["status"], "Succeeded");
    assert_eq!(lr.payload["output"]["result"].as_f64(), Some(42.0));
    eprintln!("multiply(6,7) = 42 confirmed");

    // Behavior tests
    let t = |op: &str, a: serde_json::Value| -> (String, Option<f64>) {
        let jn = Arc::new(JournalStore::in_memory().unwrap());
        register_and_enable(&*jn, &*g, &ce, "external.calculator",
            json!({"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false}),
            json!({"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false})).unwrap();
        let e = g.validate_ingress(&*jn, g.cli_ingress("t".into()).unwrap()).unwrap();
        crate::runtime::Runtime::new(kcfg(), SingleToolLlm::new(op, a)).deliver(&*jn, &*g, e).unwrap();
        let e = jn.events().unwrap();
        let r: Vec<_> = e.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
        if r.is_empty() { return ("no_receipt".into(), None); }
        (r.last().unwrap().payload["status"].as_str().unwrap_or("").to_string(),
         r.last().unwrap().payload["output"]["result"].as_f64())
    };

    assert_eq!(t("external.calculator", json!({"operation":"add","a":1,"b":2})), ("Succeeded".into(), Some(3.0)));
    assert_eq!(t("external.calculator", json!({"operation":"subtract","a":5,"b":3})), ("Succeeded".into(), Some(2.0)));
    assert_eq!(t("external.calculator", json!({"operation":"multiply","a":6,"b":7})), ("Succeeded".into(), Some(42.0)));
    assert_eq!(t("external.calculator", json!({"operation":"divide","a":8,"b":2})), ("Succeeded".into(), Some(4.0)));
    assert_eq!(t("external.calculator", json!({"operation":"divide","a":1,"b":0})).0, "Failed");
    assert_eq!(t("external.calculator", json!({"operation":"unknown","a":1,"b":2})).0, "Failed");

    eprintln!("=== E2E PASSED: S0={s0} S1={s1} proposal={pid} ===");
    let _ = std::fs::remove_dir_all(&ws_root);
    let _ = std::fs::remove_dir_all(&art_root);
    Ok(())
}
