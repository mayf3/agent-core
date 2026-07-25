//! Negative longitudinal tests for capability change decisions: rejected,
//! tampered, stale, and duplicate decisions all fail closed without changing
//! the active snapshot, registry version, or proposal status. The Proposal
//! stays PendingApproval (retryable) — a single, consistent failure semantic.

use super::capability_routes_support::*;
use crate::domain::capability_change::*;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::Result;
use rusqlite::params;
use serde_json::json;

// ── Negative longitudinal tests ────────────────────────────────────────────

#[test]
fn rejected_decision_never_activates() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;
    let v0 = registry_version(&journal);

    let body = json!({
        "decision": "rejected",
        "artifact_digest": setup.artifact_digest,
        "manifest_digest": setup.manifest_digest,
    });
    let result = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )?;
    assert_eq!(result["status"], "Rejected");

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    assert_eq!(registry_version(&journal), v0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::Rejected);
    assert_eq!(
        count_events(&journal, JournalEventKind::CapabilityChangeActivated),
        0
    );
    assert_eq!(
        count_events(&journal, JournalEventKind::RegistrySnapshotActivated),
        0
    );
    assert_eq!(
        count_events(&journal, JournalEventKind::CapabilityChangeRejected),
        1
    );
    assert_eq!(count_events(&journal, JournalEventKind::ReceiptReceived), 0);
    let snap = journal.load_registry_snapshot(&s0)?;
    assert!(snap.lookup(PROBE_OP).is_none());
    Ok(())
}

#[test]
fn tampered_artifact_blocks_activation() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;
    let v0 = registry_version(&journal);

    tamper_object(&setup.store, &setup.artifact_digest, b"tampered artifact")?;

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
    assert!(err.contains("artifact_verification_failed"), "got: {err}");

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    assert_eq!(registry_version(&journal), v0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn tampered_manifest_blocks_activation() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;

    tamper_object(&setup.store, &setup.manifest_digest, b"{\"tampered\":true}")?;

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
    assert!(err.contains("manifest_verification_failed"), "got: {err}");

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn tampered_evidence_blocks_activation() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &gw)?;
    let s0 = journal.current_registry_snapshot_id()?;

    tamper_object(&setup.store, &setup.evidence_digest, b"tampered evidence")?;

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
    assert!(err.contains("evidence_verification_failed"), "got: {err}");

    assert_eq!(journal.current_registry_snapshot_id()?, s0);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn stale_expected_snapshot_blocks_activation() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &gw)?;
    let v0 = registry_version(&journal);

    // Activate a DIFFERENT snapshot so the proposal's expected snapshot is stale.
    let snap0 = journal.load_registry_snapshot(&journal.current_registry_snapshot_id()?)?;
    let mut specs: Vec<_> = snap0.operations.iter().cloned().collect();
    specs.push(crate::registry::snapshot::OperationSpec {
        name: "external.prereg_marker".into(),
        risk: crate::registry::snapshot::Risk::ReadOnly,
        description: "marker".into(),
        parameters: json!({"type":"object"}),
        idempotent: true,
        binding_kind: crate::registry::snapshot::BindingKind::External,
        binding_key: "marker".into(),
    });
    let new_snap = journal.create_registry_snapshot(specs)?;
    journal.activate_snapshot_transactional(
        &journal.current_registry_snapshot_id()?,
        &new_snap.snapshot_id,
        "prereg",
        "marker",
    )?;

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
    assert!(err.contains("stale_expected_snapshot"), "got: {err}");

    assert_eq!(registry_version(&journal), v0 + 1);
    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    Ok(())
}

#[test]
fn duplicate_decision_is_rejected() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &gw)?;

    let body = setup.approved_body();
    let r1 = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )?;
    assert_eq!(r1["status"], "Activated");
    let activated = journal
        .load_proposal(&pid)?
        .unwrap()
        .activated_snapshot_id
        .clone();

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
    assert!(err.contains("proposal_not_pending"), "got: {err}");

    let p = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p.activated_snapshot_id, activated);
    assert_eq!(
        count_events(&journal, JournalEventKind::CapabilityChangeActivated),
        1
    );
    assert_eq!(
        count_events(&journal, JournalEventKind::RegistrySnapshotActivated),
        1
    );
    Ok(())
}
