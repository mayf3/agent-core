//! Coding Harness E2E tests — workspace ops, task state machine, and
//! full calculator development-to-42 pipeline via real capability proposal.
//!
//! All tests use real production code from `crate::harness::coding::*` and
//! the real capability proposal/approval pipeline. No mock harness implementations.
//! See calculator_e2e.rs for the full proposal → approval → activation → call flow.

mod calculator_e2e;
mod helpers;

use crate::config::KernelConfig;
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;

pub(super) fn cfg() -> KernelConfig {
    KernelConfig {
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
        harness_read_timeout_ms: 15_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ch_e2e_{}", std::process::id())),
        capability_submit_token: Some("submit-token".into()),
        capability_decision_token: Some("decision-token".into()),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 1: Workspace operations via real handler functions
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn coding_harness_workspace_ops_e2e() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ch_ws_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    use crate::harness::coding::workspace;

    let perm = crate::harness::coding::config::WorkspacePermission {
        read: true,
        write: true,
        exec: true,
        ..Default::default()
    };

    // Write a file using the real handler.
    let write_args = json!({
        "relative_path": "calc.rs",
        "content": "fn add(a:i32,b:i32)->i32{a+b}",
        "mode": "replace",
    });
    let write_resp = workspace::handle_write(&dir, &write_args);
    assert_eq!(write_resp["ok"], true, "write should succeed");
    assert!(dir.join("calc.rs").is_file(), "calc.rs should exist");

    // Read the file using the real handler.
    let read_args = json!({"relative_path": "calc.rs"});
    let read_resp = workspace::handle_read(&dir, &read_args);
    assert_eq!(read_resp["ok"], true, "read should succeed");
    assert_eq!(
        read_resp["result"]["content"],
        "fn add(a:i32,b:i32)->i32{a+b}"
    );

    // List the directory using the real handler.
    let list_args = json!({"relative_path": "."});
    let list_resp = workspace::handle_list(&dir, &list_args);
    assert_eq!(list_resp["ok"], true, "list should succeed");
    let entries = list_resp["result"]["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["name"] == "calc.rs"),
        "calc.rs should be in listing"
    );

    // Exec the rustc compiler using the real handler (compile as library, no main needed).
    let exec_args = json!({
        "command": "rustc",
        "args": ["calc.rs", "--crate-type", "lib"],
        "relative_cwd": ".",
        "timeout_seconds": 60,
        "max_output_bytes": 65536,
    });
    let exec_resp = workspace::handle_exec(&dir, &exec_args, &perm);
    assert_eq!(
        exec_resp["ok"], true,
        "compile should report ok; got: {exec_resp}"
    );
    let exit_code = exec_resp["result"]["exit_code"].as_i64().unwrap_or(-1);
    assert_eq!(
        exit_code, 0,
        "rustc exit code should be 0; stderr: {}",
        exec_resp["result"]["stderr"]
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 2: Task submit/status state machine via real handlers
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn coding_harness_task_state_machine_e2e() -> Result<()> {
    use crate::harness::coding::tasks;

    // Submit a fake-backend task.
    let submit_resp = tasks::submit_task("ws1", "build project", "build must pass", "fake");
    assert_eq!(submit_resp["result"]["status"], "queued");
    let tid = submit_resp["result"]["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Wait for execution to complete (fake backend runs in < 100ms).
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Check status → succeeded.
    let status_resp = tasks::get_status(&tid);
    assert_eq!(status_resp["result"]["status"], "succeeded");
    assert!(
        status_resp["result"]["summary"]
            .as_str()
            .unwrap_or("")
            .contains("fake"),
        "summary should contain 'fake'"
    );
    assert!(
        status_resp["result"]["commit_sha"]
            .as_str()
            .unwrap_or("")
            .contains("fake_sha"),
        "commit_sha should contain 'fake_sha'"
    );

    Ok(())
}
