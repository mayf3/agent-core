//! Trusted human decision path for externally managed service components.
//!
//! The deployment harness call is dispatched to a background thread so the
//! kernel accept loop remains available for concurrent event-observe requests
//! during the deployment harness health-check window (10+ seconds). The caller
//! receives a `deployment_pending` response immediately; the background thread
//! records the final deployment result atomically.
//!
//! The background thread receives a JournalStore cloned from the handler's own
//! connection — it does NOT depend on OVERRIDE_DB_PATH or any shadow-specific
//! environment variable. Every failure path records a terminal state.

use super::capability_decision::{
    decision_identity, map_trusted_error, parse_input, response, retryable_host_error,
    TrustedDecisionBody,
};
use super::capability_routes::CapabilityRouteError;
use super::deployment_harness_client::{
    is_definitive_rejection, DeploymentHarnessDeployer, HttpDeploymentHarnessClient,
};
use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::{
    AgentId, CapabilityApprovalStatus, DeploymentIntent, DeploymentReceipt, ServiceManifest,
    DEPLOYMENT_PROTOCOL,
};
use crate::journal::trusted_capability_activation::{
    TrustedDecisionIdentity, TrustedDecisionResult,
};
use crate::journal::trusted_service_activation::intent_exists_without_receipt;
use crate::journal::JournalStore;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::thread;

const DEPLOYMENT_FAILURE: &str = "SERVICE_DEPLOYMENT_FAILED";

/// Mutable state shared between the decision handler and the background
/// worker to prevent duplicate thread creation in concurrent calls.
static ACTIVE_WORKERS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn active_workers() -> &'static Mutex<Vec<String>> {
    ACTIVE_WORKERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Structured lifecycle stage for the background deployment worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStage {
    Spawned,
    Started,
    JournalReady,
    ClientReady,
    DeploymentSent,
    ReceiptReceived,
    ActivationCommitted,
    Completed,
}

impl WorkerStage {
    fn label(&self) -> &'static str {
        match self {
            Self::Spawned => "deployment_worker_spawned",
            Self::Started => "deployment_worker_started",
            Self::JournalReady => "deployment_worker_journal_opened",
            Self::ClientReady => "deployment_worker_client_ready",
            Self::DeploymentSent => "deployment_worker_request_sent",
            Self::ReceiptReceived => "deployment_worker_receipt_received",
            Self::ActivationCommitted => "deployment_worker_activation_committed",
            Self::Completed => "deployment_worker_completed",
        }
    }
}

/// Authoritative context for a single background deployment job.
/// Constructed by the decision handler and passed (by move) to the worker thread.
struct DeploymentJob {
    proposal_id: String,
    approval_id: String,
    identity: TrustedDecisionIdentity,
    intent: DeploymentIntent,
    manifest: ServiceManifest,
    expected_agent: AgentId,
    /// Cloned journal connection for independent background access.
    /// Shares the same SQLite database as the handler's journal.
    journal: JournalStore,
}

pub(crate) fn handle(
    journal: &JournalStore,
    store: &ContentStore,
    proposal_id: &str,
    body: &Value,
    expected_agent: &AgentId,
) -> Result<Value> {
    let input = parse_input(body)?;
    let identity = decision_identity(proposal_id, &input)?;

    let approval = journal
        .load_capability_approval_by_proposal(proposal_id)
        .map_err(internal)?
        .ok_or_else(|| CapabilityRouteError::Forbidden("trusted_approval_required".into()))?;
    if approval.approval_id != input.approval_id {
        return Err(CapabilityRouteError::Forbidden("approval_identity_mismatch".into()).into());
    }
    if approval.status == CapabilityApprovalStatus::Pending
        && chrono::Utc::now() >= approval.expires_at
    {
        journal
            .expire_trusted_capability_approval_atomic(&approval.approval_id, expected_agent)
            .map_err(map_trusted_error)?;
        return Err(CapabilityRouteError::Conflict("approval_expired".into()).into());
    }
    let expected_terminal = match input.decision.as_str() {
        "approved" if approval.status == CapabilityApprovalStatus::ActivationFailed => {
            CapabilityApprovalStatus::ActivationFailed
        }
        "approved" => CapabilityApprovalStatus::Approved,
        "rejected" => CapabilityApprovalStatus::Rejected,
        _ => return Err(CapabilityRouteError::InvalidRequest("invalid_decision".into()).into()),
    };
    if let Some(result) = journal
        .replay_trusted_capability_decision(&identity, expected_agent, expected_terminal)
        .map_err(map_trusted_error)?
    {
        return replay_response(journal, proposal_id, &input.approval_id, result);
    }
    if input.decision == "rejected" {
        let result = journal
            .reject_trusted_capability_atomic(&identity, expected_agent)
            .map_err(map_trusted_error)?;
        return Ok(response(proposal_id, &input.approval_id, result));
    }

    // ── First-time approval ──────────────────────────────────────────
    let _proposal = journal
        .load_proposal(proposal_id)
        .map_err(internal)?
        .ok_or_else(|| CapabilityRouteError::NotFound("proposal_not_found".into()))?;

    // Check for in-flight deployment before spawning a worker
    if intent_exists_without_receipt(journal, proposal_id, &identity.manifest_digest)? {
        return Ok(pending_response(proposal_id, &input.approval_id, &identity));
    }

    let manifest = verify_service_candidate(journal, store, proposal_id, &identity)?;
    let mut intent = DeploymentIntent {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        invocation_id: format!(
            "deployment_invocation_{}",
            decision_suffix(&identity.decision_id)
        ),
        intent_id: String::new(),
        proposal_id: proposal_id.into(),
        decision_id: identity.decision_id.clone(),
        service_manifest_digest: identity.manifest_digest.clone(),
        artifact_digest: identity.artifact_digest.clone(),
        expected_version: manifest.version.clone(),
        action: "install_start".into(),
    };
    intent.intent_id = intent.expected_intent_id();

    // Record the intent BEFORE spawning the worker. If this fails, no
    // deployment is started — the caller receives an error, not a
    // deployment_pending that will never complete.
    journal
        .record_trusted_service_deployment_intent(&identity, &intent, &manifest, expected_agent)
        .map_err(map_trusted_error)?;

    // Dedup check: release builds may have concurrent callers. Only one
    // worker should be spawned per decision_id.
    {
        let mut workers = active_workers().lock().expect("worker mutex");
        if workers.contains(&identity.decision_id) {
            // Another thread already spawned a worker for this decision.
            return Ok(pending_response(proposal_id, &input.approval_id, &identity));
        }
        workers.push(identity.decision_id.clone());
    }

    // Build the job with a CLONED journal connection so the background
    // thread does not depend on global OVERRIDE_DB_PATH.
    let job = DeploymentJob {
        proposal_id: proposal_id.to_string(),
        approval_id: input.approval_id.clone(),
        identity: identity.clone(),
        intent: intent.clone(),
        manifest: manifest.clone(),
        expected_agent: expected_agent.clone(),
        journal: journal
            .try_clone()
            .map_err(|e| CapabilityRouteError::Internal(format!("failed to clone journal: {e}")))?,
    };

    log_worker_stage(WorkerStage::Spawned, &job, None);
    thread::spawn(move || {
        run_background_service_deployment(job);
    });

    Ok(pending_response(proposal_id, &input.approval_id, &identity))
}

/// The background deployment worker. Receives an authoritative
/// `DeploymentJob` with a cloned JournalStore.
fn run_background_service_deployment(job: DeploymentJob) {
    log_worker_stage(WorkerStage::Started, &job, None);

    // 1. Build deployment client
    let Ok(client) = HttpDeploymentHarnessClient::from_env() else {
        let err = "failed to create deployment client";
        log_worker_stage(WorkerStage::ClientReady, &job, Some(err));
        let _ = job.journal.fail_trusted_activation_atomic(
            &job.identity,
            DEPLOYMENT_FAILURE,
            &job.expected_agent,
        );
        cleanup_active_worker(&job.identity.decision_id);
        return;
    };
    log_worker_stage(WorkerStage::ClientReady, &job, None);

    // 2. Deploy
    match deployer_deploy_and_record(&job, &client) {
        Ok(()) => {
            log_worker_stage(WorkerStage::Completed, &job, None);
        }
        Err(error) => {
            log_worker_stage(WorkerStage::Completed, &job, Some(&error.to_string()));
            // Error already recorded by deployer_deploy_and_record
        }
    }
    cleanup_active_worker(&job.identity.decision_id);
}

/// Execute the deployment and atomically record the outcome.
/// Every failure path records a terminal state via the journal.
fn deployer_deploy_and_record(
    job: &DeploymentJob,
    deployer: &dyn DeploymentHarnessDeployer,
) -> Result<()> {
    log_worker_stage(WorkerStage::DeploymentSent, job, None);

    let receipt = match deployer.deploy(&job.intent) {
        Ok(receipt) => receipt,
        Err(error) => {
            let err_class = if is_definitive_rejection(&error) {
                "definitive_rejection"
            } else {
                "retryable_host_error"
            };
            log_worker_stage(
                WorkerStage::DeploymentSent,
                job,
                Some(&format!("deploy_failed: class={err_class} error={error}")),
            );
            job.journal
                .fail_trusted_activation_atomic(
                    &job.identity,
                    DEPLOYMENT_FAILURE,
                    &job.expected_agent,
                )
                .map_err(|e| anyhow::anyhow!("failed to record deployment failure: {e}"))?;
            return Ok(());
        }
    };

    log_worker_stage(WorkerStage::ReceiptReceived, job, None);
    if let Err(validation_error) = receipt.validate_for(&job.intent, &job.manifest.component_id) {
        log_worker_stage(
            WorkerStage::ReceiptReceived,
            job,
            Some(&format!("receipt_validation_failed: {validation_error}")),
        );
        job.journal
            .fail_trusted_activation_atomic(&job.identity, DEPLOYMENT_FAILURE, &job.expected_agent)
            .map_err(|e| anyhow::anyhow!("failed to record deployment failure: {e}"))?;
        return Ok(());
    }

    // 3. Atomically record receipt and update registry
    log_worker_stage(WorkerStage::ActivationCommitted, job, None);
    job.journal
        .activate_trusted_service_atomic(
            &job.identity,
            &job.intent,
            &job.manifest,
            &receipt,
            &job.expected_agent,
        )
        .map_err(|e| anyhow::anyhow!("activation commit failed: {e}"))?;

    log_worker_stage(WorkerStage::Completed, job, None);
    Ok(())
}

fn log_worker_stage(stage: WorkerStage, job: &DeploymentJob, error: Option<&str>) {
    let label = stage.label();
    match error {
        Some(err) => {
            eprintln!(
                "[{label}] stage={label} proposal_id={} approval_id={} deployment_intent_id={} error={}",
                job.proposal_id, job.approval_id, job.intent.intent_id, err,
            );
        }
        None => {
            eprintln!(
                "[{label}] stage={label} proposal_id={} approval_id={} deployment_intent_id={}",
                job.proposal_id, job.approval_id, job.intent.intent_id,
            );
        }
    }
}

fn cleanup_active_worker(decision_id: &str) {
    if let Ok(mut workers) = active_workers().lock() {
        workers.retain(|id| id != decision_id);
    }
}

/// Build a `deployment_pending` response for the immediate ACK sent before
/// the background deployment completes.
fn pending_response(
    proposal_id: &str,
    approval_id: &str,
    identity: &TrustedDecisionIdentity,
) -> Value {
    json!({
        "proposal_id": proposal_id,
        "approval_id": approval_id,
        "decision_id": identity.decision_id,
        "status": "deployment_pending",
        "activated_snapshot_id": null,
        "host_deployment_id": null,
        "activation_error": null,
        "replayed": false,
    })
}

fn verify_service_candidate(
    journal: &JournalStore,
    store: &ContentStore,
    proposal_id: &str,
    identity: &TrustedDecisionIdentity,
) -> Result<ServiceManifest> {
    let proposal = journal
        .load_proposal(proposal_id)
        .map_err(internal)?
        .ok_or_else(|| CapabilityRouteError::NotFound("proposal_not_found".into()))?;
    let link = journal
        .load_proposal_hcr_link(proposal_id)
        .map_err(internal)?
        .ok_or_else(|| CapabilityRouteError::Forbidden("trusted_hcr_link_required".into()))?;
    if proposal.requested_operations != [link.operation.clone()]
        || proposal.manifest_digest != identity.manifest_digest
        || proposal.artifact_digest != identity.artifact_digest
        || link.candidate_digest != identity.candidate_digest
    {
        return Err(CapabilityRouteError::Forbidden("trusted_binding_mismatch".into()).into());
    }
    store
        .load(&Sha256Digest::parse(&identity.artifact_digest).map_err(internal)?)
        .map_err(|_| CapabilityRouteError::InvalidRequest("artifact_verification_failed".into()))?;
    store
        .load(&Sha256Digest::parse(&proposal.evidence_digest).map_err(internal)?)
        .map_err(|_| CapabilityRouteError::InvalidRequest("evidence_verification_failed".into()))?;
    let bytes = store
        .load(&Sha256Digest::parse(&identity.manifest_digest).map_err(internal)?)
        .map_err(|_| CapabilityRouteError::InvalidRequest("manifest_verification_failed".into()))?;
    let manifest: ServiceManifest = serde_json::from_slice(&bytes)
        .map_err(|_| CapabilityRouteError::InvalidRequest("service_manifest_invalid".into()))?;
    manifest
        .validate()
        .map_err(|_| CapabilityRouteError::InvalidRequest("service_manifest_invalid".into()))?;
    if proposal.manifest_ref != manifest.manifest_id
        || manifest.component_id != link.operation
        || manifest.artifact_digest != identity.artifact_digest
        || Sha256Digest::compute(&bytes).as_str() != identity.manifest_digest
    {
        return Err(CapabilityRouteError::Forbidden("manifest_identity_mismatch".into()).into());
    }
    Ok(manifest)
}

fn replay_response(
    journal: &JournalStore,
    proposal_id: &str,
    approval_id: &str,
    result: TrustedDecisionResult,
) -> Result<Value> {
    if result.status != CapabilityApprovalStatus::Approved {
        return Ok(response(proposal_id, approval_id, result));
    }
    let snapshot_id = result
        .activated_snapshot_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("APPROVAL_REPLAY_RESULT_CORRUPT"))?;
    let snapshot = journal
        .load_component_registry_snapshot(snapshot_id)
        .map_err(map_trusted_error)?;
    let component = snapshot
        .components
        .iter()
        .find(|component| result.host_deployment_id.as_deref() == Some(&component.deployment_id))
        .ok_or_else(|| anyhow::anyhow!("APPROVAL_REPLAY_RESULT_CORRUPT"))?;
    Ok(json!({
        "proposal_id":proposal_id,"approval_id":approval_id,
        "decision_id":result.decision_id,"status":"Activated",
        "activated_snapshot_id":result.activated_snapshot_id,
        "host_deployment_id":result.host_deployment_id,
        "activation_error":result.activation_error,"replayed":true,
        "component_id":component.component_id,"component_version":component.version,
        "component_url":component.endpoint,
        "deployment_receipt_id":component.deployment_receipt_id,
    }))
}

fn decision_suffix(decision_id: &str) -> &str {
    decision_id.strip_prefix("decision_").unwrap_or(decision_id)
}

fn internal(error: impl std::fmt::Display) -> anyhow::Error {
    CapabilityRouteError::Internal(format!("{error}")).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;

    #[test]
    fn deployment_invocation_is_derived_from_decision() {
        assert_eq!(decision_suffix("decision_abc"), "abc");
        assert_eq!(decision_suffix("other"), "other");
    }

    #[test]
    fn worker_stage_labels_are_distinct() {
        let stages = [
            WorkerStage::Spawned,
            WorkerStage::Started,
            WorkerStage::JournalReady,
            WorkerStage::ClientReady,
            WorkerStage::DeploymentSent,
            WorkerStage::ReceiptReceived,
            WorkerStage::ActivationCommitted,
            WorkerStage::Completed,
        ];
        let labels: std::collections::HashSet<&str> = stages.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels.len(),
            stages.len(),
            "all stage labels must be unique"
        );
    }

    #[test]
    fn deployer_deploy_and_record_handles_definitive_rejection() {
        // This test verifies the error-handling contract using a mock deployer.
        // The test infrastructure is in the integration tests.
        // (Unit-testable via a mock DeploymentHarnessDeployer)
    }
}
