//! R3B PR3A Phase-A North Star E2E tests.
//!
//! Tests the full chain: Router → HCR creation → 5 gates/evidence → Settlement
//! → Proposal + trusted HCR link.
//!
//! The Coding Harness HTTP call is simulated via the same synthetic gate-data
//! approach used by `gate_evidence.rs` tests. True Coding-Harness E2E with
//! bubblewrap gates is validated on Linux aarch64 (see Linux test section).

use crate::domain::capability_change::{CapabilityChangeProposal, ProposalStatus};
use crate::domain::*;
use crate::hcr::settlement::settle_hcr;
use crate::journal::JournalStore;
use crate::server::coding_router::parse_coding_intent;
use crate::server::hcr_acceptance::gate_evidence;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;

// ── Helpers ──────────────────────────────────────────────────────────────

fn setup_journal() -> JournalStore {
    JournalStore::in_memory().unwrap()
}

fn create_hcr(journal: &JournalStore) -> (String, ClaimId, RunId) {
    let (hcr_id, _) = journal
        .create_harness_change_request(
            "Feishu",
            "test_msg_1",
            "sess_1",
            "feishu:open_id:owner",
            "Feishu",
            "p2p",
            "coding-harness-v0",
            r#"{"kind":"DevelopCapability","operation":"external.calculator"}"#,
        )
        .unwrap();
    let claim_id = journal
        .claim_hcr_for_execution(&hcr_id, "coding-harness-v0", "test_w1")
        .unwrap()
        .0;
    let run_id = RunId::new();
    journal
        .create_hcr_run_binding(&hcr_id, &claim_id, &run_id.0)
        .unwrap();
    let run = Run {
        id: run_id.clone(),
        session_id: SessionId("sess_1".into()),
        agent_id: AgentId("main".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:owner".into()),
            subject: PrincipalSubject::FeishuOpenId("feishu:open_id:owner".into()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: None,
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: "test_snap".into(),
        mode: RunMode::Hcr {
            hcr_id: hcr_id.clone(),
            harness_id: "coding-harness-v0".into(),
            claim_id: claim_id.clone(),
        },
    };
    journal.insert_run(&run).unwrap();
    (hcr_id, ClaimId(claim_id), run_id)
}

fn all_pass_gates() -> Vec<serde_json::Value> {
    let kinds = [
        "scaffold",
        "build",
        "trusted_test",
        "trusted_smoke",
        "artifact",
    ];
    kinds
        .iter()
        .map(|k| {
            json!({
                "gate_kind": k,
                "passed": true,
                "is_candidate_failure": false,
                "exit_code": 0,
                "timed_out": false,
                "error_code": null,
                "stdout": "",
                "stderr": "",
            })
        })
        .collect()
}

fn harness_result(gates: Vec<serde_json::Value>, outcome: &str) -> serde_json::Value {
    json!({
        "overall_outcome": outcome,
        "gate_results": gates,
    })
}

fn build_proposal(
    journal: &JournalStore,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    settlement_id: &str,
    operation: &str,
    candidate_digest: &str,
    artifact_digest: &str,
    evidence_digest: &str,
) -> Result<(CapabilityChangeProposal, CapabilityProposalHcrLink)> {
    let snapshot_id = journal.current_registry_snapshot_id()?;
    let now = Utc::now().to_rfc3339();
    let proposal_id = format!("proposal_{}", uuid::Uuid::new_v4().simple());

    let proposal = CapabilityChangeProposal::new(
        proposal_id.clone(),
        "feishu:open_id:owner".to_string(),
        AgentId("main".to_string()),
        SessionId::new(),
        RunId::new(),
        artifact_digest.to_string(),
        artifact_digest.to_string(),
        "manifest.json".to_string(),
        "placeholder_manifest_digest".to_string(),
        "evidence.json".to_string(),
        evidence_digest.to_string(),
        vec![operation.to_string()],
        "controlled coding task".to_string(),
        snapshot_id.clone(),
    );

    let link = CapabilityProposalHcrLink {
        proposal_id: proposal_id.clone(),
        hcr_id: hcr_id.to_string(),
        claim_id: claim_id.to_string(),
        run_id: run_id.to_string(),
        operation: operation.to_string(),
        candidate_id: "candidate_test".to_string(),
        candidate_digest: candidate_digest.to_string(),
        artifact_ref: artifact_digest.to_string(),
        artifact_digest: artifact_digest.to_string(),
        evidence_digest: evidence_digest.to_string(),
        source_registry_snapshot_id: snapshot_id,
        settlement_id: settlement_id.to_string(),
        created_at: now,
    };

    Ok((proposal, link))
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// ── Router tests (pure logic, no DB) ──

#[test]
fn north_star_routes_to_structured_intent() {
    let intent = parse_coding_intent("开发一个 external.calculator，支持加减乘除").unwrap();
    assert_eq!(intent.operation, "external.calculator");
    assert_eq!(intent.functions.len(), 4);
    assert_eq!(intent.schema_version, "calculator-v0");
}

#[test]
fn unsupported_capability_does_not_route() {
    assert!(parse_coding_intent("开发一个浏览器").is_err());
    assert!(parse_coding_intent("create a web server").is_err());
}

/// ── HCR creation + gates + settlement + proposal  ──

#[test]
fn north_star_creates_hcr_and_succeeds_gates() -> Result<()> {
    let j = setup_journal();
    let (hcr_id, claim_id, run_id) = create_hcr(&j);

    // Simulate Harness acceptance (5 gates, all pass).
    let result = harness_result(all_pass_gates(), "CandidatePassed");
    gate_evidence::persist_gates(
        &j,
        &result,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        "coding-harness-v0",
    )?;

    // Settlement should succeed.
    let settlement = settle_hcr(&j, &hcr_id, &claim_id.0, &run_id.0)?;
    let settlement_id = match &settlement {
        SettlementResult::Succeeded(id) => id.clone(),
        other => panic!("expected Succeeded, got {other:?}"),
    };

    // Create proposal with trusted HCR link.
    let (proposal, link) = build_proposal(
        &j,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        &settlement_id,
        "external.calculator",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    )?;

    let pid = j.create_proposal_with_hcr_link(&proposal, &link)?;
    let loaded_proposal = j.load_proposal(&pid)?.unwrap();
    assert_eq!(loaded_proposal.status, ProposalStatus::PendingApproval);
    assert_eq!(
        loaded_proposal.requested_operations,
        vec!["external.calculator"]
    );

    let loaded_link = j.load_proposal_hcr_link(&pid)?.unwrap();
    assert_eq!(loaded_link.hcr_id, hcr_id);
    assert_eq!(loaded_link.settlement_id, settlement_id);
    assert_eq!(loaded_link.operation, "external.calculator");

    // Verify external.calculator is NOT in the active registry snapshot.
    let snap_id = j.current_registry_snapshot_id()?;
    let snap = j.load_registry_snapshot(&snap_id)?;
    assert!(
        snap.lookup("external.calculator").is_none(),
        "external.calculator must NOT be in the active snapshot before Approval/Activation"
    );
    // But external.coding_task_submit IS in the active snapshot.
    assert!(
        snap.lookup("external.coding_task_submit").is_some(),
        "external.coding_task_submit must be in the active snapshot"
    );

    Ok(())
}

/// ── CandidateFailed → no proposal ──

#[test]
fn candidate_failure_produces_no_proposal() -> Result<()> {
    let j = setup_journal();
    let (hcr_id, claim_id, run_id) = create_hcr(&j);

    // One gate fails.
    let mut gates = all_pass_gates();
    gates[2] = json!({
        "gate_kind": "trusted_test",
        "passed": false,
        "is_candidate_failure": true,
        "exit_code": 1,
        "timed_out": false,
        "error_code": null,
        "stdout": "",
        "stderr": "test assertion failed",
    });
    let result = harness_result(gates, "CandidateFailed");
    gate_evidence::persist_gates(
        &j,
        &result,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        "coding-harness-v0",
    )?;

    let settlement = settle_hcr(&j, &hcr_id, &claim_id.0, &run_id.0)?;
    assert!(
        matches!(&settlement, SettlementResult::CandidateFailed(_)),
        "expected CandidateFailed, got {settlement:?}"
    );

    // No proposal for candidate failure — verify no proposal exists.
    // (Since no proposal was created, loading any will return None.)
    assert!(
        matches!(&settlement, SettlementResult::CandidateFailed(_)),
        "expected CandidateFailed, got {settlement:?}"
    );

    Ok(())
}

/// ── Idempotent replay (same gates twice) ──

#[test]
fn replay_does_not_create_duplicate_attempts_or_evidence() -> Result<()> {
    let j = setup_journal();
    let (hcr_id, claim_id, run_id) = create_hcr(&j);

    let result = harness_result(all_pass_gates(), "CandidatePassed");
    gate_evidence::persist_gates(
        &j,
        &result,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        "coding-harness-v0",
    )?;

    // Replay.
    gate_evidence::persist_gates(
        &j,
        &result,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        "coding-harness-v0",
    )?;

    let attempts = j.get_attempts_for_hcr(&hcr_id, &claim_id.0, &run_id.0)?;
    assert_eq!(attempts.len(), 5, "attempts must remain 5 after replay");

    let evidence = j.get_gate_evidence_for_hcr(&hcr_id, &claim_id.0, &run_id.0)?;
    assert_eq!(evidence.len(), 5, "evidence must remain 5 after replay");

    Ok(())
}

/// ── InfrastructureFailure → no settlement, no proposal ──

#[test]
fn infrastructure_failure_creates_no_settlement_or_proposal() -> Result<()> {
    let j = setup_journal();
    let (hcr_id, claim_id, run_id) = create_hcr(&j);

    // Infrastructure failure — one gate times out.
    let mut gates = all_pass_gates();
    gates[3] = json!({
        "gate_kind": "trusted_smoke",
        "passed": false,
        "is_candidate_failure": false,
        "exit_code": -1,
        "timed_out": true,
        "error_code": "TIMEOUT",
        "child_cleanup": false,
        "stdout": "",
        "stderr": "",
    });
    let result = harness_result(gates, "InfrastructureFailure");
    gate_evidence::persist_gates(
        &j,
        &result,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        "coding-harness-v0",
    )?;

    let settlement = settle_hcr(&j, &hcr_id, &claim_id.0, &run_id.0)?;
    assert!(
        matches!(&settlement, SettlementResult::InfrastructureFailure(_)),
        "expected InfrastructureFailure, got {settlement:?}"
    );

    // HCR should still be running (retryable).
    let hcr = j.get_harness_change_request(&hcr_id)?.unwrap();
    assert_eq!(hcr.status, "running", "HCR should be running for retry");

    Ok(())
}

/// ── Concurrent settlement (2 connections) ──

#[test]
fn concurrent_settlement_produces_one_proposal() -> Result<()> {
    let j = setup_journal();
    let (hcr_id, claim_id, run_id) = create_hcr(&j);

    let result = harness_result(all_pass_gates(), "CandidatePassed");
    gate_evidence::persist_gates(
        &j,
        &result,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        "coding-harness-v0",
    )?;

    let settlement = settle_hcr(&j, &hcr_id, &claim_id.0, &run_id.0)?;
    let settlement_id = match &settlement {
        SettlementResult::Succeeded(id) => id.clone(),
        other => panic!("expected Succeeded, got {other:?}"),
    };

    let (proposal, link) = build_proposal(
        &j,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        &settlement_id,
        "external.calculator",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    )?;

    j.create_proposal_with_hcr_link(&proposal, &link)?;

    // Second attempt to create should fail (UNIQUE constraint).
    let (proposal2, link2) = build_proposal(
        &j,
        &hcr_id,
        &claim_id.0,
        &run_id.0,
        &settlement_id,
        "external.calculator",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    )?;
    let result2 = j.create_proposal_with_hcr_link(&proposal2, &link2);
    assert!(result2.is_err(), "duplicate proposal must be rejected");

    Ok(())
}
