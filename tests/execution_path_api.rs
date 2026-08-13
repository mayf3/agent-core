use agent_core_kernel::harness::control::{
    ApprovedHarnessChange, HarnessChangeAction, HarnessChangeIntent,
};
use agent_core_kernel::harness::manifest::HarnessManifest;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;

fn approve(
    journal: &JournalStore,
    action: HarnessChangeAction,
    manifest_id: &str,
) -> ApprovedHarnessChange {
    ApprovedHarnessChange {
        intent: HarnessChangeIntent {
            action,
            manifest_id: manifest_id.into(),
            expected_snapshot_id: journal.current_registry_snapshot_id().unwrap(),
            requested_by: "test".into(),
        },
        decision_id: "decision_execution_path_api".into(),
    }
}

#[test]
fn future_snapshot_uses_path_api_while_old_snapshot_remains_readable() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let mut legacy = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "legacy-execution-harness".into(),
        artifact_digest:
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:7650/execute".into(),
        operation_name: "external.coding_workspace_read".into(),
        description: "legacy immutable schema".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "workspace_id": {"type": "string"},
                "relative_path": {"type": "string"}
            },
            "required": ["workspace_id", "relative_path"],
            "additionalProperties": false
        }),
        output_schema: json!({"type": "object", "properties": {}}),
        idempotent: true,
        created_at: Utc::now(),
    };
    legacy.manifest_id = legacy.compute_manifest_id()?;
    journal.register_harness_manifest(&legacy)?;
    journal.enable_harness(&approve(
        &journal,
        HarnessChangeAction::Enable,
        &legacy.manifest_id,
    ))?;
    let pinned_legacy_snapshot = journal.current_registry_snapshot_id()?;

    let built = execution_harness::operation_specs::build_manifests(
        &["/source/agent-core".into()],
        "http://127.0.0.1:7650/execute",
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    for manifest in built {
        let encoded = serde_json::to_value(manifest)?;
        let manifest: HarnessManifest = serde_json::from_value(encoded)?;
        manifest.validate_all()?;
        journal.register_harness_manifest(&manifest)?;
        journal.enable_harness(&approve(
            &journal,
            HarnessChangeAction::Enable,
            &manifest.manifest_id,
        ))?;
    }
    journal.disable_harness(&approve(
        &journal,
        HarnessChangeAction::Disable,
        &legacy.manifest_id,
    ))?;

    let current = journal.load_registry_snapshot(&journal.current_registry_snapshot_id()?)?;
    for name in [
        "external.execution_list",
        "external.execution_read",
        "external.execution_write",
        "external.execution_exec",
    ] {
        let spec = current.lookup(name).expect("path operation enabled");
        let encoded = serde_json::to_string(&spec.parameters)?;
        assert!(!encoded.contains("workspace_id"));
        assert!(!encoded.contains("available_workspace_ids"));
        assert!(!encoded.contains("relative_cwd"));
        assert!(!encoded.contains("\"args\""));
    }
    assert!(current.lookup("external.coding_workspace_read").is_none());

    let historical = journal.load_registry_snapshot(&pinned_legacy_snapshot)?;
    assert!(historical
        .lookup("external.coding_workspace_read")
        .is_some());
    assert!(journal.load_harness_manifest(&legacy.manifest_id)?.is_some());
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
