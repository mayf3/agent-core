//! Trusted human decision path for externally managed service components.

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
use crate::journal::JournalStore;
use anyhow::Result;
use serde_json::{json, Value};

const DEPLOYMENT_FAILURE: &str = "SERVICE_DEPLOYMENT_FAILED";

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
    journal
        .record_trusted_service_deployment_intent(&identity, &intent, &manifest, expected_agent)
        .map_err(map_trusted_error)?;
    let client = HttpDeploymentHarnessClient::from_env().map_err(|_| retryable_host_error())?;
    handle_deployment(
        journal,
        proposal_id,
        &input,
        &identity,
        &intent,
        &manifest,
        expected_agent,
        &client,
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_deployment(
    journal: &JournalStore,
    proposal_id: &str,
    input: &TrustedDecisionBody,
    identity: &TrustedDecisionIdentity,
    intent: &DeploymentIntent,
    manifest: &ServiceManifest,
    expected_agent: &AgentId,
    deployer: &dyn DeploymentHarnessDeployer,
) -> Result<Value> {
    let receipt = match deployer.deploy(intent) {
        Ok(receipt) if receipt.validate_for(intent, &manifest.component_id).is_ok() => receipt,
        Err(error) if is_definitive_rejection(&error) => {
            let result = journal
                .fail_trusted_activation_atomic(identity, DEPLOYMENT_FAILURE, expected_agent)
                .map_err(map_trusted_error)?;
            return Ok(response(proposal_id, &input.approval_id, result));
        }
        Ok(_) | Err(_) => return Err(retryable_host_error()),
    };
    let result = journal
        .activate_trusted_service_atomic(identity, intent, manifest, &receipt, expected_agent)
        .map_err(map_trusted_error)?;
    Ok(service_response(
        proposal_id,
        &input.approval_id,
        result,
        manifest,
        &receipt,
    ))
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

fn service_response(
    proposal_id: &str,
    approval_id: &str,
    result: TrustedDecisionResult,
    manifest: &ServiceManifest,
    receipt: &DeploymentReceipt,
) -> Value {
    json!({
        "proposal_id":proposal_id,"approval_id":approval_id,
        "decision_id":result.decision_id,"status":"Activated",
        "activated_snapshot_id":result.activated_snapshot_id,
        "host_deployment_id":result.host_deployment_id,
        "activation_error":result.activation_error,"replayed":result.replayed,
        "component_id":manifest.component_id,"component_version":manifest.version,
        "component_url":receipt.endpoint,"deployment_receipt_id":receipt.receipt_id,
    })
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

    #[test]
    fn deployment_invocation_is_derived_from_decision() {
        assert_eq!(decision_suffix("decision_abc"), "abc");
        assert_eq!(decision_suffix("other"), "other");
    }
}
