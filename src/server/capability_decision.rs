//! Trusted human decision path for the fixed North Star calculator capability.

use super::capability_host_client::{
    is_definitive_rejection, CapabilityDeployRequest, CapabilityDeployResult,
    CapabilityHostDeployer, HttpCapabilityHostClient,
};
use super::capability_routes::CapabilityRouteError;
use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::{AgentId, CapabilityApprovalStatus};
use crate::harness::manifest::HarnessManifest;
use crate::journal::trusted_capability_activation::{
    TrustedDecisionIdentity, TrustedDecisionResult, TrustedHostDeployment,
};
use crate::journal::JournalStore;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

const CALCULATOR: &str = "external.calculator";
const HOST_FAILURE: &str = "CAPABILITY_HOST_DEPLOY_FAILED";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustedDecisionBody {
    decision: String,
    approval_id: String,
    decision_nonce: String,
    principal_id: String,
    expected_source_snapshot_id: String,
    candidate_digest: String,
    artifact_digest: String,
    manifest_digest: String,
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
        return Ok(response(proposal_id, &input.approval_id, result));
    }

    if input.decision == "rejected" {
        let result = journal
            .reject_trusted_capability_atomic(&identity, expected_agent)
            .map_err(map_trusted_error)?;
        return Ok(response(proposal_id, &input.approval_id, result));
    }

    let manifest = verify_candidate(journal, store, proposal_id, &identity)?;
    let target_snapshot = journal
        .trusted_calculator_prospective_snapshot(&identity, &manifest, expected_agent)
        .map_err(map_trusted_error)?;
    let request = CapabilityDeployRequest {
        protocol_version: "capability-deploy-v1".into(),
        proposal_id: proposal_id.into(),
        decision_id: identity.decision_id.clone(),
        manifest_digest: identity.manifest_digest.clone(),
        artifact_digest: identity.artifact_digest.clone(),
        target_registry_snapshot_id: target_snapshot.clone(),
    };
    let client = match HttpCapabilityHostClient::from_env() {
        Ok(client) => client,
        Err(_) => return Err(retryable_host_error()),
    };
    handle_deployment(
        journal,
        proposal_id,
        &input,
        &identity,
        &manifest,
        expected_agent,
        &target_snapshot,
        &request,
        &client,
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_deployment(
    journal: &JournalStore,
    proposal_id: &str,
    input: &TrustedDecisionBody,
    identity: &TrustedDecisionIdentity,
    manifest: &HarnessManifest,
    expected_agent: &AgentId,
    target_snapshot: &str,
    request: &CapabilityDeployRequest,
    deployer: &dyn CapabilityHostDeployer,
) -> Result<Value> {
    let deployment = match deployer.deploy(request) {
        Ok(result) if deployment_matches(&result, manifest, identity, target_snapshot) => result,
        Err(error) if is_definitive_rejection(&error) => {
            return activation_failed(journal, proposal_id, input, identity, expected_agent)
        }
        Ok(_) | Err(_) => return Err(retryable_host_error()),
    };
    let result = journal
        .activate_trusted_calculator_atomic(
            identity,
            manifest,
            &TrustedHostDeployment {
                deployment_id: deployment.deployment_id,
                target_snapshot_id: target_snapshot.into(),
            },
            expected_agent,
        )
        .map_err(map_trusted_error)?;
    Ok(response(proposal_id, &input.approval_id, result))
}

fn activation_failed(
    journal: &JournalStore,
    proposal_id: &str,
    input: &TrustedDecisionBody,
    identity: &TrustedDecisionIdentity,
    expected_agent: &AgentId,
) -> Result<Value> {
    let result = journal
        .fail_trusted_activation_atomic(identity, HOST_FAILURE, expected_agent)
        .map_err(map_trusted_error)?;
    Ok(response(proposal_id, &input.approval_id, result))
}

fn parse_input(body: &Value) -> Result<TrustedDecisionBody> {
    let input: TrustedDecisionBody = serde_json::from_value(body.clone())
        .map_err(|_| CapabilityRouteError::InvalidRequest("invalid_trusted_decision".into()))?;
    if !matches!(input.decision.as_str(), "approved" | "rejected")
        || !input.approval_id.starts_with("approval_")
        || input.approval_id.len() > 160
        || input.decision_nonce.len() < 32
        || input.decision_nonce.len() > 160
        || !input.principal_id.starts_with("feishu:open_id:")
        || input.principal_id.len() > 256
        || input.expected_source_snapshot_id.is_empty()
        || input.expected_source_snapshot_id.len() > 160
    {
        return Err(CapabilityRouteError::InvalidRequest("invalid_trusted_decision".into()).into());
    }
    for digest in [
        &input.candidate_digest,
        &input.artifact_digest,
        &input.manifest_digest,
    ] {
        Sha256Digest::parse(digest)
            .map_err(|_| CapabilityRouteError::InvalidRequest("invalid_digest_format".into()))?;
    }
    Ok(input)
}

fn decision_identity(
    proposal_id: &str,
    input: &TrustedDecisionBody,
) -> Result<TrustedDecisionIdentity> {
    let canonical = json!({
        "approval_id": input.approval_id,
        "artifact_digest": input.artifact_digest,
        "candidate_digest": input.candidate_digest,
        "decision": input.decision,
        "decision_nonce": input.decision_nonce,
        "expected_source_snapshot_id": input.expected_source_snapshot_id,
        "manifest_digest": input.manifest_digest,
        "principal_id": input.principal_id,
        "proposal_id": proposal_id,
    });
    let payload_digest = Sha256Digest::compute(&serde_json::to_vec(&canonical)?);
    let decision_id = format!(
        "decision_{}",
        payload_digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or("")
    );
    Ok(TrustedDecisionIdentity {
        proposal_id: proposal_id.into(),
        approval_id: input.approval_id.clone(),
        decision_nonce: input.decision_nonce.clone(),
        principal_id: input.principal_id.clone(),
        expected_source_snapshot_id: input.expected_source_snapshot_id.clone(),
        candidate_digest: input.candidate_digest.clone(),
        artifact_digest: input.artifact_digest.clone(),
        manifest_digest: input.manifest_digest.clone(),
        decision_id,
        payload_digest: payload_digest.as_str().into(),
    })
}

fn verify_candidate(
    journal: &JournalStore,
    store: &ContentStore,
    proposal_id: &str,
    identity: &TrustedDecisionIdentity,
) -> Result<HarnessManifest> {
    let proposal = journal
        .load_proposal(proposal_id)
        .map_err(internal)?
        .ok_or_else(|| CapabilityRouteError::NotFound("proposal_not_found".into()))?;
    let link = journal
        .load_proposal_hcr_link(proposal_id)
        .map_err(internal)?
        .ok_or_else(|| CapabilityRouteError::Forbidden("trusted_hcr_link_required".into()))?;
    if proposal.requested_operations != [CALCULATOR]
        || link.operation != CALCULATOR
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
    let manifest: HarnessManifest = serde_json::from_slice(&bytes)
        .map_err(|_| CapabilityRouteError::InvalidRequest("manifest_invalid".into()))?;
    if proposal.manifest_ref != manifest.manifest_id {
        return Err(CapabilityRouteError::Forbidden("manifest_identity_mismatch".into()).into());
    }
    Ok(manifest)
}

fn deployment_matches(
    result: &CapabilityDeployResult,
    manifest: &HarnessManifest,
    identity: &TrustedDecisionIdentity,
    target_snapshot: &str,
) -> bool {
    result.proposal_id == identity.proposal_id
        && result.decision_id == identity.decision_id
        && result.manifest_digest == identity.manifest_digest
        && result.manifest_id == manifest.manifest_id
        && result.artifact_digest == identity.artifact_digest
        && result.target_registry_snapshot_id == target_snapshot
        && result.deployment_id
            == expected_deployment_id(identity, &manifest.manifest_id, target_snapshot)
        && valid_host_id(&result.deployment_id)
        && valid_host_id(&result.probe_execution_id)
}

fn expected_deployment_id(
    identity: &TrustedDecisionIdentity,
    manifest_id: &str,
    target_snapshot: &str,
) -> String {
    let canonical = json!({
        "proposal_id": identity.proposal_id,
        "decision_id": identity.decision_id,
        "manifest_digest": identity.manifest_digest,
        "manifest_id": manifest_id,
        "artifact_digest": identity.artifact_digest,
        "operation_name": CALCULATOR,
        "target_registry_snapshot_id": target_snapshot,
    });
    let digest = Sha256Digest::compute(&serde_json::to_vec(&canonical).unwrap_or_default());
    format!(
        "chd_{}",
        digest.as_str().strip_prefix("sha256:").unwrap_or("")
    )
}

fn valid_host_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

fn response(proposal_id: &str, approval_id: &str, result: TrustedDecisionResult) -> Value {
    let status = match result.status {
        CapabilityApprovalStatus::Approved => "Activated",
        CapabilityApprovalStatus::Rejected => "Rejected",
        CapabilityApprovalStatus::ActivationFailed => "ActivationFailed",
        CapabilityApprovalStatus::Pending | CapabilityApprovalStatus::Expired => "Invalid",
    };
    json!({
        "proposal_id": proposal_id,
        "approval_id": approval_id,
        "decision_id": result.decision_id,
        "status": status,
        "activated_snapshot_id": result.activated_snapshot_id,
        "host_deployment_id": result.host_deployment_id,
        "activation_error": result.activation_error,
        "replayed": result.replayed,
    })
}

fn map_trusted_error(error: anyhow::Error) -> anyhow::Error {
    let message = error.to_string();
    if message.contains("NOT_FOUND") {
        CapabilityRouteError::NotFound("trusted_approval_not_found".into()).into()
    } else if message.contains("CONFLICT")
        || message.contains("NOT_PENDING")
        || message.contains("EXPIRED")
        || message.contains("SNAPSHOT_CHANGED")
        || message.contains("ALREADY_REGISTERED")
    {
        CapabilityRouteError::Conflict("trusted_decision_conflict".into()).into()
    } else if message.contains("MISMATCH") || message.contains("INVALID") {
        CapabilityRouteError::Forbidden("trusted_decision_mismatch".into()).into()
    } else {
        CapabilityRouteError::Internal("trusted_decision_failed".into()).into()
    }
}

fn internal(error: impl std::fmt::Display) -> anyhow::Error {
    CapabilityRouteError::Internal(format!("{error}")).into()
}

fn retryable_host_error() -> anyhow::Error {
    CapabilityRouteError::Internal("capability_host_deploy_uncertain".into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({
            "decision":"approved",
            "approval_id":"approval_123",
            "decision_nonce":"12345678901234567890123456789012",
            "principal_id":"feishu:open_id:owner",
            "expected_source_snapshot_id":"snap_0",
            "candidate_digest":format!("sha256:{}", "a".repeat(64)),
            "artifact_digest":format!("sha256:{}", "b".repeat(64)),
            "manifest_digest":format!("sha256:{}", "c".repeat(64)),
        })
    }

    #[test]
    fn decision_identity_is_deterministic_and_action_bound() {
        let parsed = parse_input(&body()).unwrap();
        let first = decision_identity("proposal_1", &parsed).unwrap();
        let second = decision_identity("proposal_1", &parsed).unwrap();
        assert_eq!(first.decision_id, second.decision_id);
        let mut changed = body();
        changed["decision"] = json!("rejected");
        let changed = decision_identity("proposal_1", &parse_input(&changed).unwrap()).unwrap();
        assert_ne!(first.decision_id, changed.decision_id);
    }

    #[test]
    fn strict_body_rejects_unknown_fields_and_bad_principal() {
        let mut unknown = body();
        unknown["artifact_ref"] = json!("attacker");
        assert!(parse_input(&unknown).is_err());
        let mut wrong_principal = body();
        wrong_principal["principal_id"] = json!("cli:local");
        assert!(parse_input(&wrong_principal).is_err());
    }

    #[test]
    fn host_result_identity_is_exact() {
        let input = parse_input(&body()).unwrap();
        let identity = decision_identity("proposal_1", &input).unwrap();
        let manifest = HarnessManifest {
            manifest_id: "manifest_1".into(),
            harness_id: String::new(),
            artifact_digest: identity.artifact_digest.clone(),
            protocol_version: String::new(),
            endpoint: String::new(),
            operation_name: String::new(),
            description: String::new(),
            input_schema: json!({}),
            output_schema: json!({}),
            idempotent: true,
            created_at: chrono::Utc::now(),
        };
        let mut result = CapabilityDeployResult {
            deployment_id: expected_deployment_id(&identity, &manifest.manifest_id, "snap_1"),
            proposal_id: identity.proposal_id.clone(),
            decision_id: identity.decision_id.clone(),
            manifest_digest: identity.manifest_digest.clone(),
            manifest_id: manifest.manifest_id.clone(),
            artifact_digest: identity.artifact_digest.clone(),
            target_registry_snapshot_id: "snap_1".into(),
            probe_execution_id: "probe_1".into(),
        };
        assert!(deployment_matches(&result, &manifest, &identity, "snap_1"));
        result.target_registry_snapshot_id = "snap_other".into();
        assert!(!deployment_matches(&result, &manifest, &identity, "snap_1"));
    }
}
