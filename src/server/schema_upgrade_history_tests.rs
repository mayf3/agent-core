//! Schema-upgrade historical preservation tests. Verifies that a schema
//! upgrade preserves the OLD manifest (so historical snapshots / Runs pinned
//! to the old snapshot keep resolving), and exercises schema-only positive
//! changes (input/output/description) directly through the decision path.

use super::super::capability_routes_support::*;
use crate::capabilities::store::Sha256Digest;
use crate::domain::*;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::{anyhow, Result};
use serde_json::json;

/// Load the artifact bytes that `setup` stored, so a schema-upgrade setup can
/// re-store them in its own content store for verification.
fn artifact_bytes_of(setup: &ProposalSetup) -> Result<Vec<u8>> {
    let digest = Sha256Digest::parse(&setup.artifact_digest)?;
    Ok(setup.store.load(&digest)?)
}

// ── Section 1: historical snapshot / old Run preservation ──────────────────

/// S0 references an old manifest; create + pin a Run to S0; perform a schema
/// upgrade to S1; assert the old manifest is still queryable by id, S0 still
/// resolves the operation to the old manifest, the pinned Run keeps S0, new
/// Runs use S1, and BOTH manifests coexist in `harness_manifests`.
#[test]
fn schema_upgrade_preserves_old_manifest_and_pinned_run() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();

    // 1. Activate the probe → S0 references the old manifest (setup1.manifest_id).
    let setup1 = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid1 = setup1.submit(&journal, &gw)?;
    handle_decision(
        &journal,
        &gw,
        &setup1.store,
        &pid1,
        &setup1.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let s0 = journal.current_registry_snapshot_id()?;
    let old_manifest_id = setup1.manifest_id.clone();

    // 2. Pin a Run to S0.
    let run_id = "run_pinned_to_s0";
    insert_pinned_run(&journal, run_id, &s0);
    assert_eq!(
        run_snapshot_id(&journal, run_id).as_deref(),
        Some(s0.as_str())
    );

    // 3. Build + activate a schema-only upgrade → S1 references the new manifest.
    let old_manifest = journal
        .load_harness_manifest(&old_manifest_id)?
        .ok_or_else(|| anyhow!("old manifest not found"))?;
    let art_bytes = artifact_bytes_of(&setup1)?;
    let up = SchemaUpgradeSetup::build(
        &old_manifest,
        &art_bytes,
        Some("Capability probe v2 (schema only)."),
        Some(
            json!({"type":"object","properties":{"new_field":{"type":"string"}},"required":["new_field"],"additionalProperties":false}),
        ),
        None,
        None,
    )?;
    let new_manifest_id = up.manifest.manifest_id.clone();
    let pid2 = up.submit(&journal, &gw)?;
    let result = handle_decision(
        &journal,
        &gw,
        &up.store,
        &pid2,
        &up.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let s1 = result["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(s1, s0);

    // 4. Old manifest is still queryable by id (immutable).
    let still_old = journal
        .load_harness_manifest(&old_manifest_id)?
        .ok_or_else(|| anyhow!("old manifest disappeared"))?;
    assert_eq!(still_old.manifest_id, old_manifest_id);

    // 5. S0 still resolves the operation to the OLD manifest.
    let snap0 = journal.load_registry_snapshot(&s0)?;
    let s0_binding = snap0.lookup(PROBE_OP).unwrap();
    assert_eq!(s0_binding.binding_key, old_manifest_id);

    // 6. The pinned Run is still on S0 — it does NOT switch to S1.
    assert_eq!(
        run_snapshot_id(&journal, run_id).as_deref(),
        Some(s0.as_str())
    );
    assert_ne!(
        run_snapshot_id(&journal, run_id).as_deref(),
        Some(s1.as_str())
    );

    // 7. New Runs would use S1 (the active snapshot is S1).
    assert_eq!(journal.current_registry_snapshot_id()?, s1);

    // 8. Both manifests coexist in harness_manifests.
    assert_eq!(manifest_count_for_operation(&journal, PROBE_OP), 2);
    assert!(manifest_exists(&journal, &old_manifest_id));
    assert!(manifest_exists(&journal, &new_manifest_id));

    // 9. S1 resolves to the new manifest.
    let snap1 = journal.load_registry_snapshot(&s1)?;
    let s1_binding = snap1.lookup(PROBE_OP).unwrap();
    assert_eq!(s1_binding.binding_key, new_manifest_id);

    // 10. Exactly one schema_upgrade RegistrySnapshotActivated event.
    assert_eq!(schema_upgrade_payloads(&journal).len(), 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ── Section 6: schema-only positive (input/output/description) ─────────────

/// A schema-only upgrade that changes ONLY the input schema must succeed.
#[test]
fn schema_upgrade_allows_input_schema_change() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup1 = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid1 = setup1.submit(&journal, &gw)?;
    handle_decision(
        &journal,
        &gw,
        &setup1.store,
        &pid1,
        &setup1.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let old_manifest = journal
        .load_harness_manifest(&setup1.manifest_id)?
        .ok_or_else(|| anyhow!("old manifest not found"))?;
    let art_bytes = artifact_bytes_of(&setup1)?;

    let new_input = json!({"type":"object","properties":{"input_field":{"type":"number"}},"required":["input_field"],"additionalProperties":false});
    let up = SchemaUpgradeSetup::build(
        &old_manifest,
        &art_bytes,
        None,
        Some(new_input.clone()),
        None,
        None,
    )?;
    let pid2 = up.submit(&journal, &gw)?;
    let result = handle_decision(
        &journal,
        &gw,
        &up.store,
        &pid2,
        &up.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    assert_eq!(result["status"], "Activated");

    // The new snapshot exposes the new input schema.
    let s1 = result["activated_snapshot_id"].as_str().unwrap();
    let snap = journal.load_registry_snapshot(s1)?;
    assert_eq!(snap.lookup(PROBE_OP).unwrap().parameters, new_input);
    // Immutable fields unchanged.
    let new_manifest = journal
        .load_harness_manifest(&up.manifest.manifest_id)?
        .unwrap();
    assert_eq!(new_manifest.endpoint, old_manifest.endpoint);
    assert_eq!(new_manifest.harness_id, old_manifest.harness_id);
    assert_eq!(new_manifest.artifact_digest, old_manifest.artifact_digest);
    assert_eq!(new_manifest.protocol_version, old_manifest.protocol_version);
    assert_eq!(new_manifest.idempotent, old_manifest.idempotent);
    Ok(())
}

/// A schema-only upgrade that changes ONLY the output schema must succeed.
#[test]
fn schema_upgrade_allows_output_schema_change() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup1 = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid1 = setup1.submit(&journal, &gw)?;
    handle_decision(
        &journal,
        &gw,
        &setup1.store,
        &pid1,
        &setup1.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let old_manifest = journal
        .load_harness_manifest(&setup1.manifest_id)?
        .ok_or_else(|| anyhow!("old manifest not found"))?;
    let art_bytes = artifact_bytes_of(&setup1)?;

    // Use a NEW manifest_id distinct from the input-change test to avoid any
    // accidental content-address collision (output differs → id differs).
    let new_output = json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"},"version":{"type":"integer"}},"required":["status","ok","version"],"additionalProperties":false});
    let up = SchemaUpgradeSetup::build(
        &old_manifest,
        &art_bytes,
        None,
        None,
        Some(new_output),
        None,
    )?;
    let pid2 = up.submit(&journal, &gw)?;
    let result = handle_decision(
        &journal,
        &gw,
        &up.store,
        &pid2,
        &up.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    assert_eq!(result["status"], "Activated");
    // New manifest differs from the old (output changed).
    assert_ne!(up.manifest.manifest_id, old_manifest.manifest_id);
    Ok(())
}

/// A schema-only upgrade that changes ONLY the description must succeed.
#[test]
fn schema_upgrade_allows_description_change() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup1 = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid1 = setup1.submit(&journal, &gw)?;
    handle_decision(
        &journal,
        &gw,
        &setup1.store,
        &pid1,
        &setup1.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let old_manifest = journal
        .load_harness_manifest(&setup1.manifest_id)?
        .ok_or_else(|| anyhow!("old manifest not found"))?;
    let art_bytes = artifact_bytes_of(&setup1)?;

    let up = SchemaUpgradeSetup::build(
        &old_manifest,
        &art_bytes,
        Some("Completely new description for the probe."),
        None,
        None,
        None,
    )?;
    let pid2 = up.submit(&journal, &gw)?;
    let result = handle_decision(
        &journal,
        &gw,
        &up.store,
        &pid2,
        &up.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    assert_eq!(result["status"], "Activated");
    let s1 = result["activated_snapshot_id"].as_str().unwrap();
    let snap = journal.load_registry_snapshot(s1)?;
    assert_eq!(
        snap.lookup(PROBE_OP).unwrap().description,
        "Completely new description for the probe."
    );
    assert_ne!(up.manifest.manifest_id, old_manifest.manifest_id);
    Ok(())
}
