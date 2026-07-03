//! Workspace harness E2E tests — full Runtime dispatch pipeline.
//! Pattern follows `external_harness_runtime.rs`.

mod coding_workspace_helpers;

use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::registry::snapshot::BindingKind;
use anyhow::Result;
use serde_json::json;

use coding_workspace_helpers::*;

fn setup_journal(ep: &str, ops: &[&str]) -> Result<(JournalStore, Gateway)> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    for op in ops {
        let mid = register_manifest(&j, ep, op)?;
        enable_op(&j, &g, &mid)?;
    }
    Ok((j, g))
}

#[test]
fn workspace_list_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wl_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f.txt"), "data\n").unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(&ep, &["external.workspace_list"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_list",
        json!({"workspace_id":"test","relative_path":"."}),
    );
    let captured = llm.captured();
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("l?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert!(caps[0]["provider_tools"]
        .as_array()
        .map(|a| a
            .iter()
            .any(|t| t["function"]["name"] == "external.workspace_list"))
        .unwrap_or(false));

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_read_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wr_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("d.txt"), "hello\n").unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(&ep, &["external.workspace_read"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_read",
        json!({"workspace_id":"test","relative_path":"d.txt"}),
    );
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("r?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_stat_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("d.txt"), "data\n").unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(&ep, &["external.workspace_stat"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_stat",
        json!({"workspace_id":"test","relative_path":"d.txt"}),
    );
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("s?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_exec_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("we_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(&ep, &["external.workspace_exec"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_exec",
        json!({
            "workspace_id":"test","program":"echo","args":["hi"],"relative_cwd":".","timeout_seconds":30,"max_output_bytes":1024
        }),
    );
    let captured = llm.captured();
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("e?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");

    let caps = captured.lock().unwrap();
    let has_tool = caps[0]["provider_tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .any(|t| t["function"]["name"] == "external.workspace_exec")
        })
        .unwrap_or(false);
    assert!(!has_tool, "Write ops excluded from provider_tools");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_mkdir_write_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wmw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _, port) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(
        &ep,
        &["external.workspace_mkdir", "external.workspace_write"],
    )?;

    // mkdir
    let llm = SingleToolLlm::new(
        "external.workspace_mkdir",
        json!({"workspace_id":"test","relative_path":"sub","recursive":false}),
    );
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("m?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    assert!(dir.join("sub").is_dir());

    // write (same mock, different journal to avoid conflicts)
    let j2 = JournalStore::in_memory()?;
    let ep2 = format!("http://127.0.0.1:{port}/execute");
    let mid = register_manifest(&j2, &ep2, "external.workspace_write")?;
    enable_op(&j2, &g, &mid)?;
    let llm = SingleToolLlm::new(
        "external.workspace_write",
        json!({"workspace_id":"test","relative_path":"hello.rs","content":"fn main(){}","mode":"replace"}),
    );
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j2, g.cli_ingress("w?".into())?)?;
    let o = rt.deliver(&j2, &g, ev)?;
    assert!(!o.output.trim().is_empty());
    let ev = j2.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    assert!(dir.join("hello.rs").is_file());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_exec_timeout() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Use high HTTP read timeout (10s) so only subprocess timeout fires.
    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let mut cfg = test_config();
    cfg.harness_read_timeout_ms = 10_000;

    let (j, g) = setup_journal(&ep, &["external.workspace_exec"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_exec",
        json!({
            "workspace_id":"test","program":"sleep","args":["30"],
            "relative_cwd":".","timeout_seconds":1,"max_output_bytes":1024
        }),
    );
    let captured = llm.captured();
    let rt = super::Runtime::new(cfg, llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("to?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[1]["follow_up_count"].as_u64().unwrap_or(0), 1);

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].payload["status"], "Failed",
        "Timeout must produce a Failed receipt"
    );
    assert_eq!(
        r[0].payload["output"]["harness_error_code"], "exec_timed_out",
        "Harness must report exec_timed_out"
    );
    assert!(
        !o.output.contains("status: succeeded"),
        "ToolResult must not indicate success"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_exec_env_isolation() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wei_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    const SENT: &str = "AGENT_CORE_TEST_SECRET_SENTINEL";
    const SENT_VAL: &str = "sentinel-do-not-leak-167";
    std::env::set_var(SENT, SENT_VAL);

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(&ep, &["external.workspace_exec"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_exec",
        json!({
            "workspace_id":"test","program":"env","args":[],
            "relative_cwd":".","timeout_seconds":10,"max_output_bytes":65536
        }),
    );
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("env?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    std::env::remove_var(SENT);

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");

    let output = &r[0].payload["output"];
    let out_str = serde_json::to_string(output).unwrap_or_default();
    assert!(
        !out_str.contains(SENT),
        "Receipt output must not contain sentinel name"
    );
    assert!(
        !out_str.contains(SENT_VAL),
        "Receipt output must not contain sentinel value"
    );
    assert!(
        !o.output.contains(SENT),
        "ToolResult must not contain sentinel name"
    );
    assert!(
        !o.output.contains(SENT_VAL),
        "ToolResult must not contain sentinel value"
    );

    let stdout = output.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        stdout.contains("PATH="),
        "PATH must be available in subprocess env"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn rejects_unknown_workspace() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ruw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(&ep, &["external.workspace_list"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_list",
        json!({"workspace_id":"unknown","relative_path":"."}),
    );
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("l?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert!(!r.is_empty());
    assert_eq!(r[0].payload["status"], "Failed");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn reject_nonexistent_program() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("rnp_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(&ep, &["external.workspace_exec"])?;
    let llm = SingleToolLlm::new(
        "external.workspace_exec",
        json!({
            "workspace_id":"test","program":"nonexistent_xyzzy","args":[],"relative_cwd":".","timeout_seconds":5,"max_output_bytes":1024
        }),
    );
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("x?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert!(!r.is_empty());
    assert_eq!(r[0].payload["status"], "Failed");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn registry_snapshot_contains_workspace_ops() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("rsw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let (j, g) = setup_journal(
        &ep,
        &[
            "external.workspace_list",
            "external.workspace_read",
            "external.workspace_write",
            "external.workspace_mkdir",
            "external.workspace_stat",
            "external.workspace_exec",
        ],
    )?;
    let sid = j.current_registry_snapshot_id()?;
    let snap = j.load_registry_snapshot(&sid)?;

    for name in &[
        "external.workspace_list",
        "external.workspace_read",
        "external.workspace_write",
        "external.workspace_mkdir",
        "external.workspace_stat",
        "external.workspace_exec",
    ] {
        let op = snap.lookup(name);
        assert!(op.is_some(), "snapshot should contain {name}");
        assert_eq!(op.unwrap().binding_kind, BindingKind::External);
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_vertical_slice() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wvs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rs"), "fn main(){}\n").unwrap();
    std::fs::write(dir.join("lib.rs"), "pub fn h(){}\n").unwrap();

    let (ep, _, _) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let mid = register_manifest(&j, &ep, "external.workspace_list")?;
    let prev = j.current_registry_snapshot_id()?;
    enable_op(&j, &g, &mid)?;
    let new = j.current_registry_snapshot_id()?;
    assert_ne!(prev, new);

    let snap = j.load_registry_snapshot(&new)?;
    assert!(snap.lookup("external.workspace_list").is_some());

    let llm = SingleToolLlm::new(
        "external.workspace_list",
        json!({"workspace_id":"test","relative_path":"."}),
    );
    let captured = llm.captured();
    let rt = super::Runtime::new(test_config(), llm);
    let ev = g.validate_ingress(&j, g.cli_ingress("l?".into())?)?;
    let o = rt.deliver(&j, &g, ev)?;
    assert!(!o.output.trim().is_empty());

    let run = j.run(&o.run_id)?.expect("run exists");
    assert_eq!(run.registry_snapshot_id, new);
    assert!(run
        .principal
        .grants
        .iter()
        .any(|g| g.operation == "external.workspace_list"));

    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert!(caps[0]["provider_tools"]
        .as_array()
        .map(|a| a
            .iter()
            .any(|t| t["function"]["name"] == "external.workspace_list"))
        .unwrap_or(false));
    assert_eq!(caps[1]["follow_up_count"].as_u64().unwrap_or(0), 1);

    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::ToolCallIssued)
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
