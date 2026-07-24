//! Read-only GET proposal endpoint tests.
//! Split from capability_routes_tests.rs to stay under the 500-line gate.

use super::capability_routes_support::*;
use crate::capabilities::store::ContentStore;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_get_proposal;
use anyhow::Result;
use rusqlite::params;

const TEST_CANDIDATE: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEST_EVIDENCE: &str =
    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

/// Insert an HCR link row (required before inserting an approval).
fn insert_hcr_link(
    journal: &JournalStore,
    proposal_id: &str,
    artifact_digest: &str,
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
            "external.capability_probe",
            "test_candidate",
            TEST_CANDIDATE,
            artifact_digest,
            artifact_digest,
            TEST_EVIDENCE,
            source_snapshot,
            "test_settlement",
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    drop(conn);
    Ok(())
}

fn insert_approval(
    journal: &JournalStore,
    proposal_id: &str,
    principal_id: &str,
    source_snapshot: &str,
    artifact_digest: &str,
    manifest_digest: &str,
) -> Result<String> {
    // HCR link must exist before approval (FOREIGN KEY constraint)
    insert_hcr_link(journal, proposal_id, artifact_digest, source_snapshot)?;
    let approval_id = format!("approval_{}", uuid::Uuid::new_v4().simple());
    let decision_nonce = format!(
        "nonce_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
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
            source_snapshot,
            TEST_CANDIDATE,
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

#[test]
fn get_proposal_returns_digests_and_status() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &_gw)?;

    let resp = handle_get_proposal(&journal, &setup.store, &pid)?;
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
    let _gw = gateway();
    let store = ContentStore::new("/tmp/nonexistent".into());
    let err = handle_get_proposal(&journal, &store, "proposal_nonexistent")
        .unwrap_err()
        .to_string();
    assert!(err.contains("not_found"), "got: {err}");
    Ok(())
}

/// Verify that GET proposal returns a complete approval object when
/// a capability_change_approvals row exists.
#[test]
fn trusted_hcr_proposal_get_returns_bound_approval() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &_gw)?;
    let source_snapshot = journal.current_registry_snapshot_id()?;
    let approval_id = insert_approval(
        &journal,
        &pid,
        "feishu:open_id:owner",
        &source_snapshot,
        &setup.artifact_digest,
        &setup.manifest_digest,
    )?;

    let resp = handle_get_proposal(&journal, &setup.store, &pid)?;
    let approval = &resp["approval"];
    assert!(
        approval.is_object(),
        "approval should be an object, got: {approval:?}"
    );
    assert_eq!(approval["approval_id"], approval_id);
    assert_eq!(approval["principal_id"], "feishu:open_id:owner");
    assert_eq!(approval["status"], "Pending");
    assert_eq!(approval["origin_channel"], "unknown");
    assert_eq!(approval["origin_conversation_kind"], "unknown");
    Ok(())
}

/// Verify that the approval's principal_id (owner_principal_id) is returned
/// and matches the identity of the user who initiated the HCR.
#[test]
fn proposal_approval_principal_matches_hcr_owner() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &_gw)?;
    let expected_principal = "feishu:open_id:hcr_requester";
    let source_snapshot = journal.current_registry_snapshot_id()?;
    insert_approval(
        &journal,
        &pid,
        expected_principal,
        &source_snapshot,
        &setup.artifact_digest,
        &setup.manifest_digest,
    )?;

    let resp = handle_get_proposal(&journal, &setup.store, &pid)?;
    assert_eq!(resp["approval"]["principal_id"], expected_principal);
    // The proposal's submitter_principal_id is "capability_submitter" (from
    // handle_submit_proposal). The approval's owner_principal_id is the
    // HCR requester. They are separate identities — the approval binds the
    // human who can decide, not the system that submitted.
    assert!(!resp["approval"]["principal_id"]
        .as_str()
        .unwrap_or("")
        .is_empty());
    Ok(())
}

/// Verify that the approval's manifest_digest matches the proposal's
/// manifest digest, confirming cross-table consistency.
#[test]
fn proposal_approval_manifest_digest_matches_proposal() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &_gw)?;
    let source_snapshot = journal.current_registry_snapshot_id()?;
    insert_approval(
        &journal,
        &pid,
        "feishu:open_id:owner",
        &source_snapshot,
        &setup.artifact_digest,
        &setup.manifest_digest,
    )?;

    let resp = handle_get_proposal(&journal, &setup.store, &pid)?;
    assert_eq!(
        resp["approval"]["manifest_digest"], resp["manifest_digest"],
        "approval manifest_digest must match proposal manifest_digest"
    );
    assert_eq!(
        resp["approval"]["artifact_digest"], resp["artifact_digest"],
        "approval artifact_digest must match proposal artifact_digest"
    );
    Ok(())
}

/// Verify that the approval's decision_nonce is present and non-empty.
#[test]
fn proposal_approval_nonce_is_present() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &_gw)?;
    let source_snapshot = journal.current_registry_snapshot_id()?;
    insert_approval(
        &journal,
        &pid,
        "feishu:open_id:owner",
        &source_snapshot,
        &setup.artifact_digest,
        &setup.manifest_digest,
    )?;

    let resp = handle_get_proposal(&journal, &setup.store, &pid)?;
    let nonce = resp["approval"]["decision_nonce"].as_str().unwrap_or("");
    assert!(
        !nonce.is_empty(),
        "decision_nonce must be present and non-empty"
    );
    assert!(
        nonce.len() >= 32,
        "decision_nonce should be at least 32 chars"
    );
    Ok(())
}
