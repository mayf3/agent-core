//! Coding Task Submit orchestrator.
//!
//! After the Coding Intent Router matches a user request, this module
//! drives the full chain:
//!   HCR creation → claim/Run binding → Harness candidate generation
//!   → PR2 acceptance (5 gates) → Settlement → CapabilityChangeProposal
//!
//! The caller is responsible for ensuring the calculator candidate files
//! exist at `candidate_dir` before calling `handle_coding_task_submit`.

use crate::config::KernelConfig;
use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::server::coding_router::CodingIntent;
use crate::server::hcr_acceptance;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::json;

/// Result of a complete coding task submit flow.
#[derive(Debug)]
pub struct CodingTaskSubmitResult {
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub harness_execution_id: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub artifact_digest: Option<String>,
    pub evidence_digest: String,
    pub settlement_id: String,
    pub proposal_id: Option<String>,
    pub proposal_link_id: Option<String>,
}

/// Run the full coding task submit chain.
///
/// 1. Create (or find existing) HCR
/// 2. Call HCR acceptance with candidate_ref
/// 3. If settlement succeeds, create CapabilityChangeProposal + trusted link
///
/// `candidate_dir`: absolute path to the calculator candidate source directory
///   (must be reachable by the coding harness from its artifact_root).
pub fn handle_coding_task_submit(
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    intent: &CodingIntent,
    principal: &RunPrincipal,
    session_id: &str,
    candidate_dir: &str,
) -> Result<CodingTaskSubmitResult> {
    // ── 1. Create (or find existing) HCR ─────────────────────────────────
    // Idempotency key: principal + spec digest
    let spec_digest = spec_digest(intent);
    let dedup_key = format!("{}_{}", principal.principal_id.0, spec_digest);
    let source_message_id = format!("dev_{}", dedup_key);

    let (hcr_id, deduplicated) = journal.create_harness_change_request(
        "CodingRouter",
        &source_message_id,
        session_id,
        &principal.principal_id.0,
        "internal",
        "p2p",
        "coding-harness-v0",
        &json!({
            "kind": "DevelopCapability",
            "operation": intent.operation,
            "functions": intent.functions,
            "schema_version": intent.schema_version,
        })
        .to_string(),
    )?;

    if deduplicated {
        // HCR already exists — check for existing settlement + proposal.
        let hcr = journal
            .get_harness_change_request(&hcr_id)?
            .ok_or_else(|| anyhow::anyhow!("HCR_NOT_FOUND_AFTER_CREATE"))?;
        if hcr.status == "succeeded" || hcr.status == "failed" {
            // Already settled — load existing settlement and check for proposal.
            // For now, return a "retry" result since detailed recovery
            // requires loading settlements by hcr_id.
            bail!("HCR_ALREADY_SETTLED_RETRY_NOT_IMPLEMENTED");
        }
        // HCR exists but not settled — caller should retry acceptance.
    }

    // ── 2. Call existing HCR acceptance handler ───────────────────────────
    let accept_body = json!({
        "candidate_ref": candidate_dir,
    });
    let accept_result = hcr_acceptance::handle(journal, gateway, config, &hcr_id, &accept_body)?;

    let outcome = accept_result["outcome"]
        .as_str()
        .unwrap_or("Unknown")
        .to_string();
    let harness_execution_id = accept_result["harness_execution_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let candidate_digest = accept_result
        .get("candidate_digest")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let artifact_digest = accept_result["artifact_digest"]
        .as_str()
        .map(|s| s.to_string());
    let evidence_digest = accept_result["evidence_digest"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let settlement_result_str = accept_result["settlement_result"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Extract settlement_id from settlement_result.
    let settlement_id = extract_settlement_id(&settlement_result_str);
    let is_succeeded = settlement_result_str.starts_with("Succeeded");

    // ── 3. Create proposal only if settlement succeeded ──────────────────
    let sid = settlement_id.clone();
    let (proposal_id, proposal_link_id) = if is_succeeded {
        let sid = sid.ok_or_else(|| anyhow::anyhow!("MISSING_SETTLEMENT_ID"))?;

        let snapshot_id = journal.current_registry_snapshot_id()?;
        let proposal = build_proposal(
            journal,
            principal,
            session_id,
            &outcome,
            &harness_execution_id,
            artifact_digest.as_deref().unwrap_or(""),
            &evidence_digest,
            &snapshot_id,
            intent,
        )?;

        // Build the HCR trusted link.
        let link = CapabilityProposalHcrLink {
            proposal_id: proposal.proposal_id.clone(),
            hcr_id: hcr_id.clone(),
            claim_id: accept_result["claim_id"].as_str().unwrap_or("").to_string(),
            run_id: accept_result["run_id"].as_str().unwrap_or("").to_string(),
            operation: intent.operation.clone(),
            candidate_id: accept_result
                .get("candidate_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            candidate_digest: candidate_digest.clone(),
            artifact_ref: artifact_digest.clone().unwrap_or_default(),
            artifact_digest: artifact_digest.clone().unwrap_or_default(),
            evidence_digest: evidence_digest.clone(),
            source_registry_snapshot_id: snapshot_id,
            settlement_id: sid.clone(),
            created_at: Utc::now().to_rfc3339(),
        };

        let pid = journal.create_proposal_with_hcr_link(&proposal, &link)?;
        (Some(pid.clone()), Some(link.proposal_id))
    } else {
        (None, None)
    };

    Ok(CodingTaskSubmitResult {
        hcr_id,
        claim_id: accept_result["claim_id"].as_str().unwrap_or("").to_string(),
        run_id: accept_result["run_id"].as_str().unwrap_or("").to_string(),
        harness_execution_id,
        candidate_id: accept_result
            .get("candidate_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        candidate_digest,
        artifact_digest,
        evidence_digest,
        settlement_id: settlement_id.unwrap_or_default(),
        proposal_id,
        proposal_link_id,
    })
}

/// Compute a deterministic spec digest for idempotency.
fn spec_digest(intent: &CodingIntent) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(intent.operation.as_bytes());
    for f in &intent.functions {
        hasher.update(b"\0");
        hasher.update(f.as_bytes());
    }
    hasher.update(b"\0");
    hasher.update(intent.schema_version.as_bytes());
    hex::encode(hasher.finalize())
}

/// Extract settlement_id from the debug format string "Succeeded(id)".
fn extract_settlement_id(s: &str) -> Option<String> {
    if s.starts_with("Succeeded(\"") || s.starts_with("CandidateFailed(\"") {
        let start = s.find('"')? + 1;
        let end = s.rfind('"')?;
        if start < end {
            return Some(s[start..end].to_string());
        }
    }
    None
}

/// Build a CapabilityChangeProposal from settlement results.
fn build_proposal(
    journal: &JournalStore,
    principal: &RunPrincipal,
    _session_id: &str,
    _outcome: &str,
    _harness_execution_id: &str,
    artifact_digest: &str,
    evidence_digest: &str,
    snapshot_id: &str,
    intent: &CodingIntent,
) -> Result<CapabilityChangeProposal> {
    let proposal_id = format!("proposal_{}", uuid::Uuid::new_v4().simple());
    let now = Utc::now();

    let expected_snapshot = if snapshot_id.is_empty() {
        journal.current_registry_snapshot_id()?
    } else {
        snapshot_id.to_string()
    };

    Ok(CapabilityChangeProposal::new(
        proposal_id,
        principal.principal_id.0.clone(),
        AgentId("main".to_string()),
        SessionId::new(),
        RunId::new(),
        artifact_digest.to_string(),               // artifact_ref
        artifact_digest.to_string(),               // artifact_digest
        "manifest.json".to_string(),               // manifest_ref
        "placeholder_manifest_digest".to_string(), // manifest_digest
        "evidence.json".to_string(),               // evidence_ref
        evidence_digest.to_string(),               // evidence_digest
        vec![intent.operation.clone()],            // requested_operations
        "controlled coding task".to_string(),      // risk_summary
        expected_snapshot,
    ))
}
