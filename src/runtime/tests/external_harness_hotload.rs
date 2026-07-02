//! External harness runtime hot-load integration tests.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
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
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        capability_submit_token: None,
        capability_decision_token: None,
    }
}

fn register_time_manifest(j: &JournalStore) -> Result<String> {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "time-harness-v1".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:7101/execute".into(),
        operation_name: "external.time_now".into(),
        description: "Return current time".into(),
        input_schema: json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
        output_schema: json!({"type": "object", "properties": {"iso": {"type": "string"}, "epoch_ms": {"type": "integer"}}, "required": ["iso", "epoch_ms"], "additionalProperties": false}),
        idempotent: true,
        created_at: Utc::now(),
    };
    let manifest_id = m.compute_manifest_id()?;
    m.manifest_id = manifest_id.clone();
    j.register_harness_manifest(&m)?;
    Ok(manifest_id)
}

#[test]
fn non_local_harness_endpoint_is_rejected() -> Result<()> {
    let m = HarnessManifest {
        manifest_id: "m1".into(),
        harness_id: "test".into(),
        artifact_digest: "sha256:1".into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://example.com:7101/execute".into(),
        operation_name: "external.test".into(),
        description: "test".into(),
        input_schema: json!({}),
        output_schema: json!({}),
        idempotent: false,
        created_at: Utc::now(),
    };
    assert!(m.validate_endpoint().is_err());
    Ok(())
}

#[test]
fn repeated_enable_is_idempotent() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let manifest_id = register_time_manifest(&j)?;

    let intent1 = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: manifest_id.clone(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    };
    let approved1 = g.approve_harness_change(intent1)?;
    let result1 = j.enable_harness(&approved1)?;
    assert!(result1.changed);

    let intent2 = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id,
        expected_snapshot_id: result1.active_snapshot_id.clone(),
        requested_by: "ipc_operator".into(),
    };
    let approved2 = g.approve_harness_change(intent2)?;
    let result2 = j.enable_harness(&approved2)?;
    assert!(!result2.changed);
    assert_eq!(result2.active_snapshot_id, result1.active_snapshot_id);
    Ok(())
}

#[test]
fn repeated_disable_is_idempotent() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let manifest_id = register_time_manifest(&j)?;

    let intent_e = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: manifest_id.clone(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    };
    let approved_e = g.approve_harness_change(intent_e)?;
    let result_e = j.enable_harness(&approved_e)?;

    let intent_d1 = HarnessChangeIntent {
        action: HarnessChangeAction::Disable,
        manifest_id: manifest_id.clone(),
        expected_snapshot_id: result_e.active_snapshot_id.clone(),
        requested_by: "ipc_operator".into(),
    };
    let approved_d1 = g.approve_harness_change(intent_d1)?;
    let result_d1 = j.disable_harness(&approved_d1)?;
    assert!(result_d1.changed);

    let intent_d2 = HarnessChangeIntent {
        action: HarnessChangeAction::Disable,
        manifest_id,
        expected_snapshot_id: result_d1.active_snapshot_id.clone(),
        requested_by: "ipc_operator".into(),
    };
    let approved_d2 = g.approve_harness_change(intent_d2)?;
    let result_d2 = j.disable_harness(&approved_d2)?;
    assert!(!result_d2.changed);
    Ok(())
}

#[test]
fn activation_compare_and_swap_rejects_stale_snapshot() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());
    let manifest_id = register_time_manifest(&j)?;

    let baseline_id = j.current_registry_snapshot_id()?;

    let intent1 = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: manifest_id.clone(),
        expected_snapshot_id: baseline_id.clone(),
        requested_by: "ipc_operator".into(),
    };
    let approved1 = g.approve_harness_change(intent1)?;
    j.enable_harness(&approved1)?;

    let intent2 = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id,
        expected_snapshot_id: baseline_id,
        requested_by: "ipc_operator".into(),
    };
    let approved2 = g.approve_harness_change(intent2)?;
    assert!(j.enable_harness(&approved2).is_err());
    Ok(())
}

#[test]
fn active_registry_snapshot_survives_restart() -> Result<()> {
    let db_path = std::env::temp_dir().join(format!("test_restart_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    let enabled_snapshot_id;
    {
        let j = JournalStore::open(&db_path)?;
        j.initialize_registry()?;
        let g = Gateway::new(test_config());
        let manifest_id = register_time_manifest(&j)?;
        let intent = HarnessChangeIntent {
            action: HarnessChangeAction::Enable,
            manifest_id,
            expected_snapshot_id: j.current_registry_snapshot_id()?,
            requested_by: "ipc_operator".into(),
        };
        let approved = g.approve_harness_change(intent)?;
        let result = j.enable_harness(&approved)?;
        enabled_snapshot_id = result.active_snapshot_id.clone();
        assert!(result.changed);
    }

    {
        let j = JournalStore::open(&db_path)?;
        j.initialize_registry()?;
        let restored = j.current_registry_snapshot_id()?;
        assert_eq!(restored, enabled_snapshot_id);

        let snap = j.load_registry_snapshot(&restored)?;
        assert!(snap.lookup("external.time_now").is_some());
    }

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

#[test]
fn external_harness_output_schema_violation_rejected() -> Result<()> {
    let m = HarnessManifest {
        manifest_id: "sv".into(),
        harness_id: "test".into(),
        artifact_digest: "sha256:sv".into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:1/execute".into(),
        operation_name: "external.sv".into(),
        description: "test".into(),
        input_schema: json!({}),
        output_schema: json!({
            "type": "object",
            "properties": {"required_field": {"type": "string"}},
            "required": ["required_field"],
            "additionalProperties": false
        }),
        idempotent: false,
        created_at: Utc::now(),
    };

    let invalid = json!({"wrong_field": 42});
    assert!(crate::registry::schema::validate_against_schema(&m.output_schema, &invalid).is_err(),);

    let extra = json!({"required_field": "ok", "extra": true});
    assert!(crate::registry::schema::validate_against_schema(&m.output_schema, &extra).is_err(),);
    Ok(())
}

#[test]
fn external_harness_enable_affects_only_future_runs() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let s1_id = j.current_registry_snapshot_id()?;
    let s1 = j.load_registry_snapshot(&s1_id)?;
    assert!(s1.lookup("external.time_now").is_none());

    let manifest_id = register_time_manifest(&j)?;
    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id,
        expected_snapshot_id: s1_id.clone(),
        requested_by: "ipc_operator".into(),
    };
    let approved = g.approve_harness_change(intent)?;
    let result = j.enable_harness(&approved)?;
    let s2_id = result.active_snapshot_id;
    assert!(result.changed);

    let s1_after = j.load_registry_snapshot(&s1_id)?;
    assert!(s1_after.lookup("external.time_now").is_none());

    let s2 = j.load_registry_snapshot(&s2_id)?;
    assert!(s2.lookup("external.time_now").is_some());
    assert_eq!(s2_id, j.current_registry_snapshot_id()?);
    Ok(())
}

#[test]
fn external_harness_disable_affects_only_future_runs() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(test_config());

    let manifest_id = register_time_manifest(&j)?;
    let s1_id = j.current_registry_snapshot_id()?;
    let intent_e = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: manifest_id.clone(),
        expected_snapshot_id: s1_id.clone(),
        requested_by: "ipc_operator".into(),
    };
    let approved_e = g.approve_harness_change(intent_e)?;
    let result_e = j.enable_harness(&approved_e)?;
    let s2_id = result_e.active_snapshot_id;

    let intent_d = HarnessChangeIntent {
        action: HarnessChangeAction::Disable,
        manifest_id,
        expected_snapshot_id: s2_id.clone(),
        requested_by: "ipc_operator".into(),
    };
    let approved_d = g.approve_harness_change(intent_d)?;
    let result_d = j.disable_harness(&approved_d)?;
    assert!(result_d.changed);
    let s3_id = result_d.active_snapshot_id;

    let s2_after = j.load_registry_snapshot(&s2_id)?;
    assert!(s2_after.lookup("external.time_now").is_some());

    let s3 = j.load_registry_snapshot(&s3_id)?;
    assert!(s3.lookup("external.time_now").is_none());
    Ok(())
}

#[test]
fn snapshot_registry_state_is_persisted() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let sid = j.current_registry_snapshot_id()?;
    let loaded = j.load_active_snapshot_from_state()?;
    assert_eq!(loaded, Some(sid));
    Ok(())
}
