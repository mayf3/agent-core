//! Manifest parser/validator decision tests. Each exercises real
//! `handle_decision` through the content-store, HarnessManifest parser,
//! operation binding, and atomic activation paths. Uses shared support
//! from the sibling `capability_routes_support` module.

use super::capability_routes_support::*;
use crate::capabilities::store::ContentStore;
use crate::domain::capability_change::*;
use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::{handle_decision, handle_submit_proposal};
use anyhow::{anyhow, Result};
use serde_json::json;

// ── Manifest parser/validator decision tests ───────────────────────────────

#[test]
fn decision_accepts_valid_external_harness_manifest() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;
    let v0 = registry_version(&journal);

    let body = setup.approved_body();
    let result = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )?;

    assert_eq!(result["status"], "Activated");
    let s1 = result["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(s1, s0);
    assert_eq!(journal.current_registry_snapshot_id()?, s1);
    assert_eq!(registry_version(&journal), v0 + 1);

    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::Activated);
    assert_eq!(p.activated_snapshot_id.as_deref(), Some(s1.as_str()));

    let snap = journal.load_registry_snapshot(&s1)?;
    assert!(snap.lookup(PROBE_OP).is_some());
    assert_eq!(
        snap.lookup(PROBE_OP).unwrap().binding_kind,
        crate::registry::snapshot::BindingKind::External
    );
    assert_eq!(
        count_events(&journal, JournalEventKind::CapabilityChangeActivated),
        1
    );
    assert_eq!(
        count_events(&journal, JournalEventKind::RegistrySnapshotActivated),
        1
    );
    assert_eq!(
        count_events(&journal, JournalEventKind::CapabilityChangeRejected),
        0
    );
    Ok(())
}

#[test]
fn decision_rejects_manifest_operation_missing() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, Some(vec!["external.other".into()]))?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;
    let v0 = registry_version(&journal);

    let body = setup.approved_body();
    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("manifest_operation_missing"), "got: {err}");

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    assert_eq!(registry_version(&journal), v0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn decision_rejects_manifest_operation_extra() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build("external.different", ENDPOINT, Some(vec![PROBE_OP.into()]))?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;

    let body = setup.approved_body();
    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("manifest_operation_missing") || err.contains("manifest_operation_extra"),
        "got: {err}"
    );

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn decision_rejects_duplicate_manifest_operation() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(
        PROBE_OP,
        ENDPOINT,
        Some(vec![PROBE_OP.into(), PROBE_OP.into()]),
    )?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;

    let body = setup.approved_body();
    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("duplicate_proposal_operation") || err.contains("manifest_operation"),
        "got: {err}"
    );

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn decision_rejects_builtin_namespace() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(
        "builtin.time_now",
        ENDPOINT,
        Some(vec!["builtin.time_now".into()]),
    )?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;

    let body = setup.approved_body();
    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("manifest_validation_failed") || err.contains("builtin_namespace"),
        "got: {err}"
    );

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn decision_rejects_development_namespace() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(
        "development.file.write",
        ENDPOINT,
        Some(vec!["development.file.write".into()]),
    )?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;

    let body = setup.approved_body();
    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("manifest_validation_failed") || err.contains("development_namespace"),
        "got: {err}"
    );

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn decision_rejects_manifest_artifact_digest_mismatch() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();

    let dir = std::env::temp_dir().join(format!(
        "cap_mismatch_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    let store = ContentStore::new(dir.join("store"));

    let artifact_a = store.store(b"artifact A")?;
    let artifact_b = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "mismatch_harness".into(),
        artifact_digest: artifact_b.into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ENDPOINT.into(),
        operation_name: PROBE_OP.into(),
        description: "mismatch probe".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"}},"required":["status","ok"],"additionalProperties":false}),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest = store.store(&manifest_bytes)?;
    let evidence_digest = store.store(b"evidence")?;

    let body = json!({
        "target_agent_id": "main",
        "artifact_ref": "a", "artifact_digest": artifact_a.as_str(),
        "manifest_ref": "m", "manifest_digest": manifest_digest.as_str(),
        "evidence_ref": "e", "evidence_digest": evidence_digest.as_str(),
        "requested_operations": [PROBE_OP],
        "risk_summary": "mismatch",
    });
    let resp = handle_submit_proposal(
        &journal,
        &gw,
        &body,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let pid = resp.proposal_id;
    let s0 = journal.current_registry_snapshot_id()?;

    let dec = json!({
        "decision": "approved",
        "artifact_digest": artifact_a.as_str(),
        "manifest_digest": manifest_digest.as_str(),
    });
    let err = handle_decision(
        &journal,
        &gw,
        &store,
        &pid,
        &dec,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("manifest_artifact_digest_mismatch"),
        "got: {err}"
    );

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn decision_rejects_existing_operation_conflict() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();

    // First: activate a probe normally so it exists in the active snapshot.
    let setup1 = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid1 = setup1.submit(&journal, &gw)?;
    let body1 = setup1.approved_body();
    handle_decision(
        &journal,
        &gw,
        &setup1.store,
        &pid1,
        &body1,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let s1 = journal.current_registry_snapshot_id()?;
    let v1 = registry_version(&journal);

    // Save the old snapshot for expected_snapshot_id in the upgrade proposal.
    let old_snapshot_id = s1.clone();

    // Second: a valid schema-only upgrade proposal (same harness, endpoint,
    // protocol, artifact — only description and schema change).
    let dir2 = std::env::temp_dir().join(format!(
        "cap_upgrade_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir2)?;
    let store2 = ContentStore::new(dir2.join("store"));
    // Use the SAME artifact digest from the first activation so the
    // artifact_verification succeeds.
    let old_manifest = journal
        .load_harness_manifest(&setup1.manifest_id)?
        .ok_or_else(|| anyhow!("old manifest not found"))?;
    let artifact_digest = old_manifest.artifact_digest.clone();
    // Store the same artifact content for verification.
    let art_bytes = setup1
        .store
        .load(&crate::capabilities::store::Sha256Digest::parse(
            &artifact_digest,
        )?)?;
    store2.store(&art_bytes)?;
    let evidence_digest = store2.store(br#"{"attestation":"schema-upgrade-v2"}"#)?;
    let mut manifest2 = HarnessManifest {
        manifest_id: String::new(),
        harness_id: old_manifest.harness_id.clone(),
        artifact_digest: artifact_digest.clone(),
        protocol_version: old_manifest.protocol_version.clone(),
        endpoint: old_manifest.endpoint.clone(),
        operation_name: PROBE_OP.into(),
        description: "Capability probe v2 (schema only).".into(),
        input_schema: json!({"type":"object","properties":{"new_field":{"type":"string"}},"required":["new_field"],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"}},"required":["status","ok"],"additionalProperties":false}),
        idempotent: old_manifest.idempotent,
        created_at: chrono::Utc::now(),
    };
    manifest2.manifest_id = manifest2.compute_manifest_id()?;
    let manifest_bytes = serde_json::to_vec(&manifest2)?;
    let manifest_digest = store2.store(&manifest_bytes)?;

    let body2 = json!({
        "target_agent_id": "main",
        "artifact_ref": "a", "artifact_digest": artifact_digest,
        "manifest_ref": "m", "manifest_digest": manifest_digest.as_str(),
        "evidence_ref": "e", "evidence_digest": evidence_digest.as_str(),
        "requested_operations": [PROBE_OP],
        "risk_summary": "schema upgrade probe",
    });
    let resp2 = handle_submit_proposal(
        &journal,
        &gw,
        &body2,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )?;

    // The proposal must use the OLD snapshot_id so expected_snapshot works.
    let pid2 = resp2.proposal_id;

    let dec2 = json!({
        "decision": "approved",
        "artifact_digest": artifact_digest,
        "manifest_digest": manifest_digest.as_str(),
    });

    // Schema-only upgrade should succeed.
    let upgrade_result = handle_decision(
        &journal,
        &gw,
        &store2,
        &pid2,
        &dec2,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let new_snap_id = upgrade_result["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_snap_id, old_snapshot_id, "snapshot must change");
    let new_snap = journal.load_registry_snapshot(&new_snap_id)?;
    let upgraded = new_snap.lookup(PROBE_OP).unwrap();
    assert_eq!(
        upgraded.binding_key, manifest2.manifest_id,
        "binding_key must be new manifest_id"
    );
    assert_eq!(registry_version(&journal), v1 + 1);

    // 3. Endpoint change must be rejected.
    let dir3 = std::env::temp_dir().join(format!(
        "cap_upgrade_bad_endpoint_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir3)?;
    let store3 = ContentStore::new(dir3.join("store"));
    store3.store(&art_bytes)?;
    let ev3 = store3.store(br#"{"attestation":"bad-endpoint"}"#)?;
    let mut manifest3 = manifest2.clone();
    manifest3.endpoint = "http://127.0.0.1:9999".into();
    manifest3.manifest_id = manifest3.compute_manifest_id()?;
    let m3_bytes = serde_json::to_vec(&manifest3)?;
    let md3 = store3.store(&m3_bytes)?;
    let body3 = json!({
        "target_agent_id": "main", "artifact_ref": "a",
        "artifact_digest": artifact_digest,
        "manifest_ref": "m", "manifest_digest": md3.as_str(),
        "evidence_ref": "e", "evidence_digest": ev3.as_str(),
        "requested_operations": [PROBE_OP], "risk_summary": "bad endpoint",
    });
    let resp3 = handle_submit_proposal(
        &journal,
        &gw,
        &body3,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let pid3 = resp3.proposal_id;
    let dec3 = json!({
        "decision": "approved",
        "artifact_digest": artifact_digest,
        "manifest_digest": md3.as_str(),
    });
    let err = handle_decision(
        &journal,
        &gw,
        &store3,
        &pid3,
        &dec3,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("endpoint_changed"),
        "expected endpoint rejection, got: {err}"
    );

    // Snapshot should still be S2 (from the successful upgrade), not advanced.
    assert_eq!(journal.current_registry_snapshot_id()?, new_snap_id);
    assert_eq!(registry_version(&journal), v1 + 1);
    let p3 = journal.load_proposal(&pid3)?.unwrap();
    assert_eq!(p3.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn get_proposal_returns_digests_and_status() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &_gw)?;

    let resp = crate::server::capability_routes::handle_get_proposal(&journal, &setup.store, &pid)?;
    assert_eq!(resp["proposal_id"], pid);
    assert_eq!(resp["status"], "PendingApproval");
    assert_eq!(resp["operation_name"], PROBE_OP);
    assert!(!resp["artifact_digest"].as_str().unwrap_or("").is_empty());
    assert!(!resp["manifest_digest"].as_str().unwrap_or("").is_empty());
    assert!(!resp["manifest_id"].as_str().unwrap_or("").is_empty());
    Ok(())
}

#[test]
fn get_proposal_not_found_returns_error() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let store = ContentStore::new("/tmp/nonexistent".into());
    let err = crate::server::capability_routes::handle_get_proposal(
        &journal,
        &store,
        "proposal_nonexistent",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("not_found"), "got: {err}");
    Ok(())
}
