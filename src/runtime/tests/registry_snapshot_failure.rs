//! G — Registry snapshot failure tests.
//!
//! G1 — Current snapshot missing → deliver fails cleanly.
//! G2 — Current snapshot ID points to nonexistent snapshot → deliver fails cleanly.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::runtime::Runtime;
use anyhow::Result;
use std::path::PathBuf;

// ---- Fixtures ----

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from("."),
        agent_id: AgentId("main".into()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: String::new(),
        ipc_token: "test".into(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: String::new(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        max_tool_rounds: 12,
        capability_submit_token: None,
        capability_decision_token: None,
    }
}
// G1 — Current snapshot missing → deliver fails cleanly
// ========================================================================

#[test]
fn g1_current_snapshot_missing_deliver_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let err = match runtime.deliver(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!err.is_empty(), "deliver must fail when no snapshot exists");
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error must contain registry_snapshot_unavailable, got: {err}"
    );

    // SessionReady is written before the snapshot check, so event count
    // increases by 1 even on failure. Run count must not change.
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady event should be written before snapshot check"
    );
    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");

    Ok(())
}

#[test]
fn g1_current_snapshot_missing_echo_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let llm = crate::llm::LocalEchoLlm;

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let runtime = Runtime::new(config, llm);
    let err = match runtime.deliver_echo(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!err.is_empty(), "deliver_echo must fail when no snapshot");
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error must contain registry_snapshot_unavailable, got: {err}"
    );
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady event"
    );
    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");
    Ok(())
}

// ========================================================================
// G2 — Snapshot ID points to nonexistent snapshot → deliver fails cleanly
// ========================================================================

#[test]
fn g2_current_snapshot_dangling_deliver_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    journal.set_current_snapshot_id_for_test(
        "snap_nonexistent_00000000000000000000000000000000000000000000",
    );
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let err = match runtime.deliver(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(
        !err.is_empty(),
        "deliver must fail when snapshot is dangling"
    );
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error must mention snapshot failure, got: {err}"
    );

    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    // SessionReady is written before the snapshot check.
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady event should be written before snapshot check"
    );
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");

    Ok(())
}

#[test]
fn g2_current_snapshot_dangling_echo_fails_cleanly() -> Result<()> {
    let journal = JournalStore::in_memory_without_registry()?;
    journal.set_current_snapshot_id_for_test(
        "snap_nonexistent_00000000000000000000000000000000000000000000",
    );
    let config = test_config();
    let gateway = Gateway::new(config.clone());

    let envelope = gateway.cli_ingress("hi".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let event_count_before = journal.event_count()?;
    let run_count_before = journal.run_count()?;
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);
    let err = match runtime.deliver_echo(&journal, &gateway, event) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!err.is_empty(), "deliver_echo must fail");
    assert!(
        err.contains("registry_snapshot_unavailable"),
        "error: {err}"
    );
    assert_eq!(
        journal.event_count()?,
        event_count_before + 1,
        "SessionReady"
    );
    assert_eq!(journal.run_count()?, run_count_before, "No new Run");
    assert_eq!(journal.running_run_count()?, 0, "No Running runs");
    Ok(())
}
