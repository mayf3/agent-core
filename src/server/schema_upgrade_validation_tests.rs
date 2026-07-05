//! Schema-upgrade validation rejection tests. Each test exercises ONE
//! disallowed change through the real `handle_decision` path and asserts the
//! full fail-closed invariant set: proposal stays PendingApproval, active
//! snapshot unchanged, registry version unchanged, no successful
//! RegistrySnapshotActivated(schema_upgrade), no partial writes, stable
//! error category.

use super::super::capability_routes_support::*;
use crate::capabilities::store::Sha256Digest;
use crate::domain::capability_change::ProposalStatus;
use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::{anyhow, Result};
use serde_json::json;

/// Load the artifact bytes a setup stored so an upgrade setup can re-store them.
fn artifact_bytes_of(setup: &ProposalSetup) -> Result<Vec<u8>> {
    let digest = Sha256Digest::parse(&setup.artifact_digest)?;
    Ok(setup.store.load(&digest)?)
}

/// Common precondition: activate the probe so a schema-upgrade target exists.
struct UpgradeTarget {
    journal: JournalStore,
    old_manifest: HarnessManifest,
    artifact_bytes: Vec<u8>,
    active_snapshot: String,
    version: i64,
}

fn upgrade_target() -> Result<UpgradeTarget> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid_create = setup.submit(&journal, &gw)?;
    handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid_create,
        &setup.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let old_manifest = journal
        .load_harness_manifest(&setup.manifest_id)?
        .ok_or_else(|| anyhow!("old manifest not found"))?;
    let artifact_bytes = artifact_bytes_of(&setup)?;
    let active_snapshot = journal.current_registry_snapshot_id()?;
    let version = registry_version(&journal);
    Ok(UpgradeTarget {
        journal,
        old_manifest,
        artifact_bytes,
        active_snapshot,
        version,
    })
}

/// Run a rejection case and assert the complete fail-closed invariant set.
/// `build_upgrade` receives the active old manifest and its artifact bytes,
/// and returns the (deliberately invalid) upgrade setup.
fn assert_rejected<F>(target: UpgradeTarget, expected_err: &[&str], build_upgrade: F) -> Result<()>
where
    F: FnOnce(&HarnessManifest, &[u8]) -> Result<SchemaUpgradeSetup>,
{
    let UpgradeTarget {
        journal,
        old_manifest,
        artifact_bytes,
        active_snapshot,
        version,
    } = target;
    let gw = gateway();
    let up = build_upgrade(&old_manifest, &artifact_bytes)?;
    let pid = up.submit(&journal, &gw)?;
    let before_events = schema_upgrade_payloads(&journal).len();

    let err = handle_decision(
        &journal,
        &gw,
        &up.store,
        &pid,
        &up.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    let matched = expected_err.iter().any(|frag| err.contains(frag));
    assert!(matched, "expected one of {expected_err:?}, got: {err}");

    // Proposal stays PendingApproval (no activation, no partial write).
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    assert!(p.activated_snapshot_id.is_none());

    // Active snapshot unchanged.
    assert_eq!(journal.current_registry_snapshot_id()?, active_snapshot);
    // Registry version unchanged.
    assert_eq!(registry_version(&journal), version);
    // No new successful schema_upgrade event.
    assert_eq!(schema_upgrade_payloads(&journal).len(), before_events);
    // The invalid new manifest is NOT present in the table (no partial write).
    assert!(!manifest_exists(&journal, &up.manifest.manifest_id));
    // Hash chain still valid.
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ── 5.1 endpoint changed ───────────────────────────────────────────────────

#[test]
fn schema_upgrade_rejects_endpoint_change() -> Result<()> {
    let target = upgrade_target()?;
    assert_rejected(target, &["endpoint_changed"], |old, art| {
        SchemaUpgradeSetup::build(
            old,
            art,
            None,
            None,
            None,
            Some(&|m: &mut HarnessManifest| {
                // Loopback + explicit port, but DIFFERENT from the original.
                m.endpoint = "http://127.0.0.1:19000/probe".into();
            }),
        )
    })
}

// ── 5.2 artifact_digest changed ────────────────────────────────────────────

#[test]
fn schema_upgrade_rejects_artifact_digest_change() -> Result<()> {
    let target = upgrade_target()?;
    // Point the manifest at a forged digest that differs from the proposal's
    // (real) artifact_digest. The handler binds the decision's artifact_digest
    // to the proposal's digest first (artifact_digest_mismatch), so the
    // artifact change is rejected before reaching schema-only immutable-field
    // checks. Either stable error proves the artifact change is rejected.
    assert_rejected(
        target,
        &[
            "artifact_changed",
            "manifest_artifact_digest_mismatch",
            "artifact_digest_mismatch",
        ],
        |old, art| {
            SchemaUpgradeSetup::build(
                old,
                art,
                None,
                None,
                None,
                Some(&|m: &mut HarnessManifest| {
                    m.artifact_digest =
                        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .into();
                }),
            )
        },
    )
}

// ── 5.3 harness_id changed ─────────────────────────────────────────────────

#[test]
fn schema_upgrade_rejects_harness_id_change() -> Result<()> {
    let target = upgrade_target()?;
    assert_rejected(target, &["harness_changed"], |old, art| {
        SchemaUpgradeSetup::build(
            old,
            art,
            None,
            None,
            None,
            Some(&|m: &mut HarnessManifest| {
                m.harness_id = "different_harness_id".into();
            }),
        )
    })
}

// ── 5.4 protocol_version changed ───────────────────────────────────────────

#[test]
fn schema_upgrade_rejects_protocol_version_change() -> Result<()> {
    let target = upgrade_target()?;
    // The manifest validator (validate_protocol_version) rejects an invalid
    // protocol_version before the schema-only immutable-field checks run. The
    // rejection is therefore a manifest_validation_failed carrying the
    // protocol_version fragment — a stable, explicit error.
    assert_rejected(
        target,
        &[
            "protocol_changed",
            "manifest_validation_failed",
            "protocol_version",
        ],
        |old, art| {
            SchemaUpgradeSetup::build(
                old,
                art,
                None,
                None,
                None,
                Some(&|m: &mut HarnessManifest| {
                    m.protocol_version = "external-harness-v2".into();
                }),
            )
        },
    )
}

// ── 5.5 idempotent changed ─────────────────────────────────────────────────

#[test]
fn schema_upgrade_rejects_idempotent_change() -> Result<()> {
    let target = upgrade_target()?;
    assert_rejected(target, &["idempotent_changed"], |old, art| {
        SchemaUpgradeSetup::build(
            old,
            art,
            None,
            None,
            None,
            Some(&|m: &mut HarnessManifest| {
                // Old manifest is idempotent=true; flip to false.
                m.idempotent = !old.idempotent;
            }),
        )
    })
}

// ── 5.6 non-External operation (Builtin) ───────────────────────────────────

/// Attempt a schema upgrade whose target is a Builtin operation. The handler
/// must reject cleanly: a manifest cannot declare a non-`external.` operation
/// (validate_operation_name), so any attempt to upgrade `system.status` fails
/// closed with no state change.
#[test]
fn schema_upgrade_rejects_non_external_operation() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();

    // The baseline snapshot contains builtin `system.status`. A proposal that
    // requests it cannot match a manifest (manifests must use external.*), so
    // the rejection fires at the operation-set check.
    let active = journal.current_registry_snapshot_id()?;
    let version = registry_version(&journal);

    // Build a real, valid manifest that exposes external.nonext_marker, and a
    // proposal whose requested_operations target the BUILTIN system.status.
    // Manifests cannot declare a non-`external.` operation, so any attempt to
    // upgrade a Builtin op resolves to manifest_operation_missing here.
    let dir = std::env::temp_dir().join(format!(
        "cap_nonext_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    let store = crate::capabilities::store::ContentStore::new(dir.join("store"));
    let artifact_bytes = b"#!/bin/sh\necho nonexternal\n";
    let artifact_digest = store.store(artifact_bytes)?;
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "nonext_harness".into(),
        artifact_digest: artifact_digest.as_str().into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ENDPOINT.into(),
        operation_name: "external.nonext_marker".into(),
        description: "non-external marker".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"}},"required":["status","ok"],"additionalProperties":false}),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest = store.store(&manifest_bytes)?;
    let evidence_digest = store.store(br#"{"attestation":"nonext"}"#)?;

    let body = json!({
        "target_agent_id": "main",
        "artifact_ref": "artifact.bin",
        "artifact_digest": artifact_digest.as_str(),
        "manifest_ref": "manifest.json",
        "manifest_digest": manifest_digest.as_str(),
        "evidence_ref": "evidence.json",
        "evidence_digest": evidence_digest.as_str(),
        "requested_operations": ["system.status"],
        "risk_summary": "upgrade builtin",
    });
    let resp = crate::server::capability_routes::handle_submit_proposal(
        &journal,
        &gw,
        &body,
        "capability_submitter",
        &AgentId("main".to_string()),
    )?;
    let pid = resp.proposal_id;
    let dec = json!({
        "decision": "approved",
        "artifact_digest": artifact_digest.as_str(),
        "manifest_digest": manifest_digest.as_str(),
    });
    let err = handle_decision(
        &journal,
        &gw,
        &store,
        &pid,
        &dec,
        "approval_workflow",
        &AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("manifest_operation_missing") || err.contains("builtin_namespace"),
        "expected non-external rejection, got: {err}"
    );

    // Full fail-closed invariant set.
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    assert!(p.activated_snapshot_id.is_none());
    assert_eq!(journal.current_registry_snapshot_id()?, active);
    assert_eq!(registry_version(&journal), version);
    assert_eq!(schema_upgrade_payloads(&journal).len(), 0);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
