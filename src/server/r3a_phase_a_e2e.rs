//! PR3A security tests that do not simulate Harness success.
//!
//! The successful five-gate path lives in Coding Harness's Linux-only real
//! E2E.  These Kernel tests cover routing and fail-closed Proposal derivation.

use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use crate::journal::JournalStore;
use crate::server::coding_router::parse_coding_intent;
use chrono::Utc;

const A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

#[test]
fn north_star_routes_to_structured_intent() {
    let intent = parse_coding_intent("开发一个 external.calculator，支持加减乘除").unwrap();
    assert_eq!(intent.operation, "external.calculator");
    assert_eq!(intent.functions, ["add", "subtract", "multiply", "divide"]);
    assert_eq!(intent.schema_version, "calculator-v0");
}

#[test]
fn unsupported_capability_does_not_route() {
    assert!(parse_coding_intent("开发一个浏览器").is_err());
    assert!(parse_coding_intent("create a web server").is_err());
}

#[test]
fn forged_settlement_link_cannot_create_proposal() {
    let journal = JournalStore::in_memory().unwrap();
    let snapshot = journal.current_registry_snapshot_id().unwrap();
    let proposal = CapabilityChangeProposal::new(
        "proposal_forged".into(),
        "feishu:open_id:owner".into(),
        AgentId("main".into()),
        SessionId("session_forged".into()),
        RunId("run_forged".into()),
        B.into(),
        B.into(),
        "manifest_forged".into(),
        A.into(),
        C.into(),
        C.into(),
        vec!["external.calculator".into()],
        "forged".into(),
        snapshot.clone(),
    );
    let link = CapabilityProposalHcrLink {
        proposal_id: proposal.proposal_id.clone(),
        hcr_id: "hcr_forged".into(),
        claim_id: "claim_forged".into(),
        run_id: "run_hcr_forged".into(),
        operation: "external.calculator".into(),
        candidate_id: "candidate_forged".into(),
        candidate_digest: A.into(),
        artifact_ref: B.into(),
        artifact_digest: B.into(),
        evidence_digest: C.into(),
        source_registry_snapshot_id: snapshot,
        settlement_id: "settlement_forged".into(),
        created_at: Utc::now().to_rfc3339(),
    };
    let error = journal
        .create_proposal_with_hcr_link(&proposal, &link)
        .unwrap_err()
        .to_string();
    assert!(error.contains("TRUSTED_HCR_NOT_FOUND"), "{error}");
    assert!(journal.load_proposal("proposal_forged").unwrap().is_none());
}

#[test]
fn caller_cannot_substitute_artifact_or_operation() {
    let journal = JournalStore::in_memory().unwrap();
    let snapshot = journal.current_registry_snapshot_id().unwrap();
    let proposal = CapabilityChangeProposal::new(
        "proposal_substitute".into(),
        "owner".into(),
        AgentId("main".into()),
        SessionId("session".into()),
        RunId("run".into()),
        B.into(),
        B.into(),
        "manifest".into(),
        A.into(),
        C.into(),
        C.into(),
        vec!["external.calculator".into()],
        "test".into(),
        snapshot.clone(),
    );
    let link = CapabilityProposalHcrLink {
        proposal_id: proposal.proposal_id.clone(),
        hcr_id: "hcr".into(),
        claim_id: "claim".into(),
        run_id: "run_hcr".into(),
        operation: "external.shell".into(),
        candidate_id: "candidate".into(),
        candidate_digest: A.into(),
        artifact_ref: A.into(),
        artifact_digest: A.into(),
        evidence_digest: C.into(),
        source_registry_snapshot_id: snapshot,
        settlement_id: "settlement".into(),
        created_at: Utc::now().to_rfc3339(),
    };
    let error = journal
        .create_proposal_with_hcr_link(&proposal, &link)
        .unwrap_err()
        .to_string();
    assert!(error.contains("PROPOSAL_LINK_FIELD_MISMATCH"));
}

#[test]
fn baseline_contains_controls_but_not_calculator() {
    let journal = JournalStore::in_memory().unwrap();
    let snapshot = journal
        .load_registry_snapshot(&journal.current_registry_snapshot_id().unwrap())
        .unwrap();
    assert!(snapshot.lookup("external.coding_task_submit").is_some());
    assert!(snapshot.lookup("external.coding_hcr_accept").is_some());
    assert!(snapshot.lookup("external.calculator").is_none());
}
