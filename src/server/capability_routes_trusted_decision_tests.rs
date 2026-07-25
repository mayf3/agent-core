//! Trusted calculator / HCR decision negative tests: missing and tampered approvals.
//! Split from capability_routes_negative_tests.rs to stay under the 500-line gate.

use super::capability_routes_support::*;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::Result;
use rusqlite::params;
use serde_json::json;

const CALCULATOR_OP: &str = "external.calculator";
const CALCULATOR_ENDPOINT: &str = "http://127.0.0.1:18999/calculator";
const TEST_AGENT: &str = "main";
const CANDIDATE_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const EVIDENCE_DIGEST: &str =
    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

/// Insert a `capability_proposal_hcr_links` row for test purposes.
fn insert_hcr_link(
    journal: &JournalStore,
    proposal_id: &str,
    candidate_digest: &str,
    artifact_digest: &str,
    evidence_digest: &str,
    source_snapshot: &str,
) -> Result<()> {
    let hcr_id = format!("hcr_{}", uuid::Uuid::new_v4().simple());
    let conn = journal.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    conn.execute(
        "INSERT INTO capability_proposal_hcr_links
         (proposal_id,hcr_id,claim_id,run_id,operation,
          candidate_id,candidate_digest,artifact_ref,artifact_digest,
          evidence_digest,source_registry_snapshot_id,settlement_id,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            proposal_id,
            hcr_id,
            "test_claim",
            "test_run",
            CALCULATOR_OP,
            "test_candidate",
            candidate_digest,
            artifact_digest,
            artifact_digest,
            evidence_digest,
            source_snapshot,
            "test_settlement",
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    drop(conn);
    Ok(())
}

/// Insert a `capability_change_approvals` row for the calculator test chain.
fn insert_calculator_approval(
    journal: &JournalStore,
    proposal_id: &str,
    principal_id: &str,
    snapshot_id: &str,
    candidate_digest: &str,
    artifact_digest: &str,
    manifest_digest: &str,
) -> Result<String> {
    let approval_id = format!("approval_{}", uuid::Uuid::new_v4().simple());
    let decision_nonce = format!(
        "nonce_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
    );
    let conn = journal.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    conn.execute(
        "INSERT INTO capability_change_approvals
         (approval_id,proposal_id,owner_principal_id,source_registry_snapshot_id,
          candidate_digest,artifact_digest,manifest_digest,decision_nonce,status,
          created_at,expires_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'Pending',?9,?10)",
        params![
            approval_id,
            proposal_id,
            principal_id,
            snapshot_id,
            candidate_digest,
            artifact_digest,
            manifest_digest,
            decision_nonce,
            chrono::Utc::now().to_rfc3339(),
            (chrono::Utc::now() + chrono::TimeDelta::hours(1)).to_rfc3339(),
        ],
    )?;
    drop(conn);
    Ok(approval_id)
}

/// Build a full calculator test chain: proposal + HCR link + approval + content.
/// Returns (proposal_id, approval_id, decision_nonce, source_snapshot_id, setup).
fn build_calculator_chain(
    journal: &JournalStore,
    _gw: &Gateway,
    requested_ops: Option<Vec<String>>,
) -> Result<(String, String, String, String, ProposalSetup)> {
    let setup = ProposalSetup::build(CALCULATOR_OP, CALCULATOR_ENDPOINT, requested_ops)?;
    let pid = setup.submit(journal, _gw)?;
    let source_snapshot = journal.current_registry_snapshot_id()?;

    insert_hcr_link(
        journal,
        &pid,
        CANDIDATE_DIGEST,
        &setup.artifact_digest,
        EVIDENCE_DIGEST,
        &source_snapshot,
    )?;

    let approval_id = insert_calculator_approval(
        journal,
        &pid,
        "feishu:open_id:owner",
        &source_snapshot,
        CANDIDATE_DIGEST,
        &setup.artifact_digest,
        &setup.manifest_digest,
    )?;

    // Retrieve the nonce we just inserted by loading the approval
    let approval = journal
        .load_capability_approval_by_proposal(&pid)?
        .ok_or_else(|| anyhow::anyhow!("approval not found"))?;
    let decision_nonce = approval.decision_nonce;

    Ok((pid, approval_id, decision_nonce, source_snapshot, setup))
}

/// A valid TrustedDecisionBody that matches the calculator test chain.
fn calculator_approved_body(
    approval_id: &str,
    decision_nonce: &str,
    source_snapshot: &str,
    setup: &ProposalSetup,
) -> serde_json::Value {
    json!({
        "decision": "approved",
        "approval_id": approval_id,
        "decision_nonce": decision_nonce,
        "principal_id": "feishu:open_id:owner",
        "expected_source_snapshot_id": source_snapshot,
        "candidate_digest": CANDIDATE_DIGEST,
        "artifact_digest": setup.artifact_digest,
        "manifest_digest": setup.manifest_digest,
    })
}

/// A proposal with requested_operations = ["external.calculator"] but NO
/// approval record must be rejected with trusted_approval_required.
#[test]
fn calculator_decision_missing_approval_is_rejected() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let setup = ProposalSetup::build(
        CALCULATOR_OP,
        CALCULATOR_ENDPOINT,
        Some(vec![CALCULATOR_OP.into()]),
    )?;
    let pid = setup.submit(&journal, &gw)?;

    let source_snapshot = journal.current_registry_snapshot_id()?;
    insert_hcr_link(
        &journal,
        &pid,
        CANDIDATE_DIGEST,
        &setup.artifact_digest,
        EVIDENCE_DIGEST,
        &source_snapshot,
    )?;

    let body = json!({
        "decision": "approved",
        "approval_id": "approval_nonexistent",
        "decision_nonce": "nonce_".to_string() + &"x".repeat(56),
        "principal_id": "feishu:open_id:owner",
        "expected_source_snapshot_id": source_snapshot,
        "candidate_digest": CANDIDATE_DIGEST,
        "artifact_digest": setup.artifact_digest,
        "manifest_digest": setup.manifest_digest,
    });

    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId(TEST_AGENT.into()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("trusted_approval_required") || err.contains("not_found"),
        "expected approval missing error, got: {err}"
    );
    Ok(())
}

/// Send a decision with approval_id that does not match the stored approval.
#[test]
fn tampered_approval_id_is_rejected() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let (pid, _real_aid, nonce, snapshot, setup) =
        build_calculator_chain(&journal, &gw, Some(vec![CALCULATOR_OP.into()]))?;

    let body = calculator_approved_body("approval_wrong", &nonce, &snapshot, &setup);

    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId(TEST_AGENT.into()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("approval_identity_mismatch"),
        "expected approval_identity_mismatch, got: {err}"
    );
    Ok(())
}

/// Decision with a principal_id that does not match the approval owner.
#[test]
fn trusted_decision_principal_must_match_approval_owner() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let (pid, aid, nonce, snapshot, setup) =
        build_calculator_chain(&journal, &gw, Some(vec![CALCULATOR_OP.into()]))?;

    let mut body = calculator_approved_body(&aid, &nonce, &snapshot, &setup);
    body["principal_id"] = json!("feishu:open_id:attacker");

    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId(TEST_AGENT.into()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("trusted_decision_mismatch") || err.contains("MISMATCH"),
        "expected binding mismatch for wrong principal, got: {err}"
    );
    Ok(())
}

/// Decision with a decision_nonce that does not match the stored approval.
#[test]
fn tampered_decision_nonce_is_rejected() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let (pid, aid, _real_nonce, snapshot, setup) =
        build_calculator_chain(&journal, &gw, Some(vec![CALCULATOR_OP.into()]))?;

    let wrong_nonce = format!("nonce_wrong_{}", uuid::Uuid::new_v4().simple());
    let body = calculator_approved_body(&aid, &wrong_nonce, &snapshot, &setup);

    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId(TEST_AGENT.into()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("trusted_decision_mismatch") || err.contains("MISMATCH"),
        "expected binding mismatch for wrong nonce, got: {err}"
    );
    Ok(())
}

/// Decision with a manifest_digest that does not match the proposal.
#[test]
fn tampered_manifest_digest_is_rejected() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let (pid, aid, nonce, snapshot, setup) =
        build_calculator_chain(&journal, &gw, Some(vec![CALCULATOR_OP.into()]))?;

    let mut body = calculator_approved_body(&aid, &nonce, &snapshot, &setup);
    body["manifest_digest"] =
        json!("sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");

    let err = handle_decision(
        &journal,
        &gw,
        &setup.store,
        &pid,
        &body,
        "approval_workflow",
        &crate::domain::AgentId(TEST_AGENT.into()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("trusted_decision_mismatch")
            || err.contains("MISMATCH")
            || err.contains("binding_mismatch"),
        "expected binding mismatch for wrong manifest digest, got: {err}"
    );
    Ok(())
}
