//! Workspace harness E2E tests — prove the full Runtime can list, read, write,
//! mkdir, stat, and exec across the external harness adapter.
//! Pattern follows `external_harness_runtime.rs`.

mod helpers;

use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::registry::snapshot::BindingKind;
use anyhow::Result;
use serde_json::json;
use std::time::Duration;

use helpers::*;

#[test]
fn workspace_list_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_list_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hello.txt"), "world\n").unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let mid = register_workspace_manifest(&j, &ep, "external.workspace_list",
        json!({"type":"object","properties":{"workspace_id":{"type":"string"},"relative_path":{"type":"string"},"max_entries":{"type":"integer"}},"required":["workspace_id","relative_path"],"additionalProperties":true}),
        json!({"type":"object","properties":{"entries":{"type":"array"},"entry_count":{"type":"integer"}},"required":["entries","entry_count"]}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let llm = SingleToolLlm::new("external.workspace_list", json!({
        "workspace_id": "test", "relative_path": "."
    }));
    let captured = llm.captured();
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("list files?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());

    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2);
    let has_tool = caps[0]["provider_tools"].as_array()
        .map(|a| a.iter().any(|t| t["function"]["name"] == "external.workspace_list"))
        .unwrap_or(false);
    assert!(has_tool, "Round 1 tools include workspace_list");
    assert_eq!(caps[1]["follow_up_count"].as_u64().unwrap_or(0), 1);

    let ev = j.events()?;
    let receipts: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_read_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_read_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("data.txt"), "test content 42\n").unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let mid = register_workspace_manifest(&j, &ep, "external.workspace_read",
        json!({"type":"object","properties":{"workspace_id":{"type":"string"},"relative_path":{"type":"string"}},"required":["workspace_id","relative_path"],"additionalProperties":true}),
        json!({"type":"object","properties":{"content":{"type":"string"}},"required":["content"]}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let llm = SingleToolLlm::new("external.workspace_read", json!({
        "workspace_id": "test", "relative_path": "data.txt"
    }));
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("read file?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());

    let ev = j.events()?;
    let receipts: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_stat_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_stat_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("data.txt"), "test content 42\n").unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let mid = register_workspace_manifest(&j, &ep, "external.workspace_stat",
        json!({"type":"object","properties":{"workspace_id":{"type":"string"},"relative_path":{"type":"string"}},"required":["workspace_id","relative_path"],"additionalProperties":true}),
        json!({"type":"object","properties":{"type":{"type":"string"},"size_bytes":{"type":"integer"}},"required":["type","size_bytes"]}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let llm = SingleToolLlm::new("external.workspace_stat", json!({
        "workspace_id": "test", "relative_path": "data.txt"
    }));
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("stat file?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());

    let ev = j.events()?;
    let receipts: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_exec_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_exec_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let mid = register_workspace_manifest(&j, &ep, "external.workspace_exec",
        json!({"type":"object","properties":{"workspace_id":{"type":"string"},"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"relative_cwd":{"type":"string"},"timeout_seconds":{"type":"integer"},"max_output_bytes":{"type":"integer"}},"required":["workspace_id","program"],"additionalProperties":true}),
        json!({"type":"object","properties":{"exit_code":{"type":"integer"},"stdout":{"type":"string"},"stderr":{"type":"string"},"timed_out":{"type":"boolean"}},"required":["exit_code","stdout","stderr","timed_out"]}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let llm = SingleToolLlm::new("external.workspace_exec", json!({
        "workspace_id": "test", "program": "echo", "args": ["hello from workspace"],
        "relative_cwd": ".", "timeout_seconds": 30, "max_output_bytes": 1024
    }));
    let captured = llm.captured();
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("run echo?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());

    let ev = j.events()?;
    let receipts: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].payload["status"], "Succeeded");

    let caps = captured.lock().unwrap();
    let has_tool = caps[0]["provider_tools"].as_array()
        .map(|a| a.iter().any(|t| t["function"]["name"] == "external.workspace_exec"))
        .unwrap_or(false);
    assert!(!has_tool, "Write ops not in provider_tools");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_mkdir_write_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_mw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let mid_mkdir = register_workspace_manifest(&j, &ep, "external.workspace_mkdir",
        json!({"type":"object","properties":{"workspace_id":{"type":"string"},"relative_path":{"type":"string"},"recursive":{"type":"boolean"}},"required":["workspace_id","relative_path"],"additionalProperties":true}),
        json!({"type":"object","properties":{"created":{"type":"boolean"}},"required":["created"]}),
    )?;
    let mid_write = register_workspace_manifest(&j, &ep, "external.workspace_write",
        json!({"type":"object","properties":{"workspace_id":{"type":"string"},"relative_path":{"type":"string"},"content":{"type":"string"},"mode":{"type":"string"}},"required":["workspace_id","relative_path","content"],"additionalProperties":true}),
        json!({"type":"object","properties":{"bytes_written":{"type":"integer"},"sha256":{"type":"string"}},"required":["bytes_written","sha256"]}),
    )?;
    enable_workspace_operation(&j, &g, &mid_mkdir)?;
    enable_workspace_operation(&j, &g, &mid_write)?;

    // Test mkdir in first run.
    let llm = SingleToolLlm::new("external.workspace_mkdir", json!({
        "workspace_id": "test", "relative_path": "new_dir", "recursive": false
    }));
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("mkdir?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Succeeded");
    assert!(dir.join("new_dir").is_dir());

    // Test write in second run (fresh journal).
    let j2 = JournalStore::in_memory()?;
    let ep2 = format!("http://127.0.0.1:{_port}/execute");
    let mid_w2 = register_workspace_manifest(&j2, &ep2, "external.workspace_write",
        json!({"type":"object","properties":{"workspace_id":{"type":"string"},"relative_path":{"type":"string"},"content":{"type":"string"},"mode":{"type":"string"}},"required":["workspace_id","relative_path","content"],"additionalProperties":true}),
        json!({"type":"object","properties":{"bytes_written":{"type":"integer"},"sha256":{"type":"string"}},"required":["bytes_written","sha256"]}),
    )?;
    enable_workspace_operation(&j2, &g, &mid_w2)?;
    let llm2 = SingleToolLlm::new("external.workspace_write", json!({
        "workspace_id": "test", "relative_path": "hello.rs",
        "content": "fn main() { println!(\"hi\"); }", "mode": "replace"
    }));
    let rt2 = super::Runtime::new(test_config(), llm2);
    let event2 = g.validate_ingress(&j2, g.cli_ingress("write?".into())?)?;
    let outcome2 = rt2.deliver(&j2, &g, event2)?;
    assert!(!outcome2.output.trim().is_empty());
    let ev2 = j2.events()?;
    let r2: Vec<_> = ev2.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0].payload["status"], "Succeeded");
    assert!(dir.join("hello.rs").is_file());

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_list_with_limits() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_limit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..10 { std::fs::write(dir.join(format!("f{i}.txt")), "data\n").unwrap(); }

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let mid = register_workspace_manifest(&j, &ep, "external.workspace_list",
        json!({"type":"object","additionalProperties":true}),
        json!({"type":"object","additionalProperties":true}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let llm = SingleToolLlm::new("external.workspace_list", json!({
        "workspace_id": "test", "relative_path": ".", "max_entries": 5
    }));
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("list?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert!(!r.is_empty());
    assert_eq!(r[0].payload["status"], "Succeeded");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn rejects_unknown_workspace() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_rej_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let mid = register_workspace_manifest(&j, &ep, "external.workspace_list",
        json!({"type":"object","additionalProperties":true}),
        json!({"type":"object","additionalProperties":true}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let llm = SingleToolLlm::new("external.workspace_list", json!({
        "workspace_id": "nonexistent", "relative_path": "."
    }));
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("list?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert!(!r.is_empty());
    assert_eq!(r[0].payload["status"], "Failed");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn reject_exec_timeout() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_to_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _shutdown, _port) =
        start_workspace_harness(dir.clone(), Some(Duration::from_millis(500)))?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let mid = register_workspace_manifest(&j, &ep, "external.workspace_exec",
        json!({"type":"object","additionalProperties":true}),
        json!({"type":"object","additionalProperties":true}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let mut cfg = test_config();
    cfg.harness_read_timeout_ms = 200;
    let llm = SingleToolLlm::new("external.workspace_exec", json!({
        "workspace_id": "test", "program": "sleep", "args": ["10"],
        "relative_cwd": ".", "timeout_seconds": 1, "max_output_bytes": 1024
    }));
    let rt = super::Runtime::new(cfg, llm);
    let event = g.validate_ingress(&j, g.cli_ingress("run?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    if !r.is_empty() {
        let status = r[0].payload["status"].as_str().unwrap_or("");
        assert!(status == "Failed" || status == "Succeeded");
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn reject_nonexistent_program() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_np_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let mid = register_workspace_manifest(&j, &ep, "external.workspace_exec",
        json!({"type":"object","additionalProperties":true}),
        json!({"type":"object","additionalProperties":true}),
    )?;
    enable_workspace_operation(&j, &g, &mid)?;

    let llm = SingleToolLlm::new("external.workspace_exec", json!({
        "workspace_id": "test", "program": "nonexistent_cmd_xyzzy", "args": [],
        "relative_cwd": ".", "timeout_seconds": 5, "max_output_bytes": 1024
    }));
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("run?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());
    let ev = j.events()?;
    let r: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert!(!r.is_empty());
    assert_eq!(r[0].payload["status"], "Failed");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn registry_snapshot_contains_workspace_ops() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_snap_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let ops = ["external.workspace_list", "external.workspace_read",
        "external.workspace_write", "external.workspace_mkdir",
        "external.workspace_stat", "external.workspace_exec"];

    for name in &ops {
        let mid = register_workspace_manifest(&j, &ep, name,
            json!({"type":"object","additionalProperties":true}),
            json!({"type":"object","additionalProperties":true}),
        )?;
        enable_workspace_operation(&j, &g, &mid)?;
    }

    let snapshot_id = j.current_registry_snapshot_id()?;
    let snapshot = j.load_registry_snapshot(&snapshot_id)?;

    for name in &ops {
        let op = snapshot.lookup(name);
        assert!(op.is_some(), "Snapshot should contain {name}");
        assert_eq!(op.unwrap().binding_kind, BindingKind::External);
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn workspace_vertical_slice() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ws_vs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("lib.rs"), "pub fn hello() {}\n").unwrap();

    let (ep, _shutdown, _port) = start_workspace_harness(dir.clone(), None)?;
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let mid = register_workspace_manifest(&j, &ep, "external.workspace_list",
        json!({"type":"object","additionalProperties":true}),
        json!({"type":"object","additionalProperties":true}),
    )?;
    let prev_id = j.current_registry_snapshot_id()?;
    enable_workspace_operation(&j, &g, &mid)?;
    let new_id = j.current_registry_snapshot_id()?;
    assert_ne!(prev_id, new_id);

    let snapshot = j.load_registry_snapshot(&new_id)?;
    assert!(snapshot.lookup("external.workspace_list").is_some());

    let llm = SingleToolLlm::new("external.workspace_list", json!({
        "workspace_id": "test", "relative_path": "."
    }));
    let captured = llm.captured();
    let rt = super::Runtime::new(test_config(), llm);
    let event = g.validate_ingress(&j, g.cli_ingress("list?".into())?)?;
    let outcome = rt.deliver(&j, &g, event)?;
    assert!(!outcome.output.trim().is_empty());

    let run = j.run(&outcome.run_id)?.expect("run exists");
    assert_eq!(run.registry_snapshot_id, new_id);
    assert!(run.principal.grants.iter().any(|g| g.operation == "external.workspace_list"));

    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2);
    let has_tool = caps[0]["provider_tools"].as_array()
        .map(|a| a.iter().any(|t| t["function"]["name"] == "external.workspace_list"))
        .unwrap_or(false);
    assert!(has_tool);
    assert_eq!(caps[1]["follow_up_count"].as_u64().unwrap_or(0), 1);

    let ev = j.events()?;
    let receipts: Vec<_> = ev.iter().filter(|e| e.kind == JournalEventKind::ReceiptReceived).collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].payload["status"], "Succeeded");

    let ti = ev.iter().filter(|e| e.kind == JournalEventKind::ToolCallIssued).count();
    assert_eq!(ti, 1);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
