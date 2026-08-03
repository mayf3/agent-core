use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::config::KernelConfig;
use crate::contract_catalog::ContractCatalog;
use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::{CodingTaskSubmissionClaim, JournalStore};
use crate::server::coding_harness_client;
use crate::server::coding_harness_client::CodingHarnessExecutionOutcome;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};

const PROFILE_CATALOG_VERSION: &str = "component-profile-catalog-v1";

#[derive(Debug, Clone, serde::Serialize)]
pub struct CodingTaskSubmitResult {
    pub development_request_id: String,
    pub contract_catalog_version: String,
    pub component_profile: String,
    pub submit_invocation_id: String,
    pub acceptance_receipt_digest: String,
    pub acceptance_issuer: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub artifact_ref: String,
    pub artifact_digest: String,
    pub evidence_digest: String,
    pub proposal_id: String,
}

/// Execute the generic submit → external profile acceptance → Proposal chain.
/// No HCR row, claim, run binding, gate projection or settlement is created.
pub fn handle_coding_task_submit(
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    request: &DevelopmentRequest,
    run: &Run,
    session: &Session,
    source_message_id: &str,
    submission_call_key: &str,
) -> Result<CodingTaskSubmitResult> {
    handle_coding_task_submit_with(
        journal,
        gateway,
        config,
        request,
        run,
        session,
        source_message_id,
        submission_call_key,
        &coding_harness_client::execute,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_coding_task_submit_with(
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    request: &DevelopmentRequest,
    run: &Run,
    session: &Session,
    source_message_id: &str,
    submission_call_key: &str,
    execute: &impl Fn(&ApprovedInvocation, std::time::Duration) -> CodingHarnessExecutionOutcome,
) -> Result<CodingTaskSubmitResult> {
    ContractCatalog::v1().validate_request(request)?;
    validate_private_owner_context(config.feishu_coding_owner_id.as_deref(), run, session)?;
    validate_source_binding(request, run, session, source_message_id)?;
    let snapshot = journal.load_registry_snapshot(&run.registry_snapshot_id)?;
    let request_digest = digest_json(request)?;
    let proposed_attempt_id = format!("attempt_{}", uuid::Uuid::new_v4().simple());
    // The attempt is also the external invocation identity.  Both are created
    // by the Kernel and are never accepted from model arguments.
    let proposed_invocation = InvocationId(proposed_attempt_id.clone());
    let claim = journal.claim_coding_task_submission(
        source_message_id,
        submission_call_key,
        &request_digest,
        &proposed_attempt_id,
        &proposed_invocation,
        &run.id,
        &session.id,
    )?;
    let (submit_invocation, result) = match claim {
        CodingTaskSubmissionClaim::InProgress => bail!("CODING_TASK_ALREADY_IN_PROGRESS"),
        CodingTaskSubmissionClaim::Succeeded {
            invocation_id,
            result,
        } => (invocation_id, result),
        CodingTaskSubmissionClaim::DefinitivelyRejected { error_code } => {
            bail!("CODING_HARNESS_DEFINITIVELY_REJECTED:{error_code}")
        }
        CodingTaskSubmissionClaim::Claimed {
            attempt_id,
            invocation_id,
        } => {
            let attempt_key = format!("development-attempt:{attempt_id}");
            match execute_new_submission(
                journal,
                gateway,
                config,
                run,
                session,
                &snapshot,
                &invocation_id,
                &attempt_key,
                request,
                execute,
            ) {
                Ok(CodingHarnessExecutionOutcome::Succeeded(result)) => {
                    journal.complete_coding_task_submission(
                        &attempt_id,
                        &invocation_id,
                        &result,
                    )?;
                    (invocation_id, result)
                }
                Ok(CodingHarnessExecutionOutcome::DefinitivelyRejected { error_code }) => {
                    journal.reject_coding_task_submission(
                        &attempt_id,
                        &invocation_id,
                        &error_code,
                    )?;
                    bail!("CODING_HARNESS_DEFINITIVELY_REJECTED:{error_code}");
                }
                Ok(CodingHarnessExecutionOutcome::OutcomeUnknown(error)) | Err(error) => {
                    journal
                        .mark_coding_task_submission_outcome_unknown(&attempt_id, &invocation_id)?;
                    return Err(error.context("CODING_HARNESS_OUTCOME_UNKNOWN"));
                }
            }
        }
    };
    let accepted = validate_acceptance(&result, request, &request_digest, &submit_invocation.0)?;
    let store = ContentStore::new(config.harness_artifact_root.clone());
    for digest in [
        &accepted.artifact_digest,
        &accepted.evidence_digest,
        &accepted.manifest_digest,
    ] {
        store.load(&Sha256Digest::parse(digest)?)?;
    }

    let proposal_id = format!("proposal_{}", uuid::Uuid::new_v4().simple());
    let proposal = CapabilityChangeProposal::new(
        proposal_id.clone(),
        run.principal.principal_id.0.clone(),
        run.agent_id.clone(),
        session.id.clone(),
        run.id.clone(),
        accepted.artifact_ref.clone(),
        accepted.artifact_digest.clone(),
        accepted.manifest_ref.clone(),
        accepted.manifest_digest.clone(),
        accepted.evidence_digest.clone(),
        accepted.evidence_digest.clone(),
        vec![request.name.clone()],
        format!(
            "{}; external profile {} accepted",
            request.request_id, request.build_profile
        ),
        run.registry_snapshot_id.clone(),
    );
    let link = CapabilityProposalReceiptLink {
        proposal_id,
        request_id: request.request_id.clone(),
        request_digest,
        acceptance_invocation_id: submit_invocation.0.clone(),
        issuer_principal_id: accepted.issuer.clone(),
        operation: request.name.clone(),
        candidate_id: accepted.candidate_id.clone(),
        candidate_digest: accepted.candidate_digest.clone(),
        artifact_ref: accepted.artifact_ref.clone(),
        artifact_digest: accepted.artifact_digest.clone(),
        manifest_ref: accepted.manifest_ref.clone(),
        manifest_digest: accepted.manifest_digest.clone(),
        evidence_digest: accepted.evidence_digest.clone(),
        receipt_digest: accepted.receipt_digest.clone(),
        acceptance_outcome: "passed".into(),
        contract_catalog_version: request.contract_catalog_version.clone(),
        profile_id: request.build_profile.clone(),
        profile_catalog_version: PROFILE_CATALOG_VERSION.into(),
        source_registry_snapshot_id: run.registry_snapshot_id.clone(),
        origin_run_id: run.id.0.clone(),
        origin_session_id: session.id.0.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let proposal_id = journal.create_proposal_with_receipt(&proposal, &link)?;
    Ok(CodingTaskSubmitResult {
        development_request_id: request.request_id.clone(),
        contract_catalog_version: request.contract_catalog_version.clone(),
        component_profile: request.build_profile.clone(),
        submit_invocation_id: submit_invocation.0,
        acceptance_receipt_digest: accepted.receipt_digest,
        acceptance_issuer: accepted.issuer,
        candidate_id: accepted.candidate_id,
        candidate_digest: accepted.candidate_digest,
        artifact_ref: accepted.artifact_ref,
        artifact_digest: accepted.artifact_digest,
        evidence_digest: accepted.evidence_digest,
        proposal_id,
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_new_submission(
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    run: &Run,
    session: &Session,
    snapshot: &crate::registry::snapshot::RegistrySnapshot,
    invocation_id: &InvocationId,
    submit_key: &str,
    request: &DevelopmentRequest,
    execute: &impl Fn(&ApprovedInvocation, std::time::Duration) -> CodingHarnessExecutionOutcome,
) -> Result<CodingHarnessExecutionOutcome> {
    use super::invocable::{append_invocation_approved, append_invocation_proposed};
    let intent = InvocationIntent {
        invocation_id: invocation_id.clone(),
        run_id: run.id.clone(),
        operation: crate::domain::operation::external::TASK_SUBMIT.into(),
        arguments: json!({
            "session_id": session.id.0,
            "development_request": request,
            "idempotency_key": submit_key,
        }),
        idempotency_key: Some(submit_key.into()),
    };
    append_invocation_proposed(journal, run, session, &intent)?;
    let approved = gateway.approve_invocation(intent, run, session, snapshot)?;
    append_invocation_approved(journal, run, session, &approved)?;
    let outcome = execute(
        &approved,
        std::time::Duration::from_millis(config.harness_read_timeout_ms.max(900_000)),
    );
    match &outcome {
        CodingHarnessExecutionOutcome::Succeeded(result) => {
            journal.append_event(
                JournalEventKind::ReceiptReceived,
                Some(&run.id),
                Some(&session.id),
                Some(&invocation_id.0),
                json!({
                    "invocation_id": invocation_id.0,
                    "operation": crate::domain::operation::external::TASK_SUBMIT,
                    "status": "Succeeded",
                    "outcome": "succeeded",
                    "acceptance_receipt_digest": result.get("receipt_digest"),
                }),
            )?;
        }
        CodingHarnessExecutionOutcome::DefinitivelyRejected { error_code } => {
            journal.append_event(
                JournalEventKind::ReceiptReceived,
                Some(&run.id),
                Some(&session.id),
                Some(&invocation_id.0),
                json!({
                    "invocation_id": invocation_id.0,
                    "operation": crate::domain::operation::external::TASK_SUBMIT,
                    "status": "Failed",
                    "outcome": "definitively_rejected",
                    "error_code": error_code,
                }),
            )?;
        }
        CodingHarnessExecutionOutcome::OutcomeUnknown(_) => {}
    }
    Ok(outcome)
}

pub(super) struct AcceptedCandidate {
    issuer: String,
    receipt_digest: String,
    candidate_id: String,
    candidate_digest: String,
    artifact_ref: String,
    artifact_digest: String,
    manifest_ref: String,
    manifest_digest: String,
    evidence_digest: String,
}

pub(super) fn validate_acceptance(
    value: &Value,
    request: &DevelopmentRequest,
    request_digest: &str,
    invocation_id: &str,
) -> Result<AcceptedCandidate> {
    if required(value, "request_id")? != request.request_id
        || required(value, "request_digest")? != request_digest
        || required(value, "contract_catalog_version")? != request.contract_catalog_version
        || required(value, "profile_id")? != request.build_profile
        || required(value, "profile_catalog_version")? != PROFILE_CATALOG_VERSION
        || required(value, "acceptance_outcome")? != "passed"
    {
        bail!("ACCEPTANCE_REQUEST_BINDING_MISMATCH");
    }
    let envelope: ExternalReceiptEnvelope = serde_json::from_value(
        value
            .get("acceptance_receipt")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("ACCEPTANCE_RECEIPT_MISSING"))?,
    )?;
    envelope
        .validate_structure()
        .map_err(|error| anyhow::anyhow!(error))?;
    envelope
        .verify_receipt_digest()
        .map_err(|error| anyhow::anyhow!(error))?;
    let candidate_digest = digest(value, "candidate_digest")?;
    let artifact_digest = digest(value, "artifact_digest")?;
    let manifest_digest = digest(value, "manifest_digest")?;
    let evidence_digest = digest(value, "evidence_digest")?;
    let binding = compute_acceptance_binding_digest(
        request_digest,
        &candidate_digest,
        &artifact_digest,
        &manifest_digest,
        ExternalOutcome::Passed,
        &request.contract_catalog_version,
        &request.build_profile,
        PROFILE_CATALOG_VERSION,
    );
    if envelope.invocation_intent_id != invocation_id
        || envelope.issuer != TRUSTED_ACCEPTANCE_ISSUER
        || envelope.outcome != ExternalOutcome::Passed
        || envelope.subject_digest != artifact_digest
        || envelope.evidence_digest != evidence_digest
        || envelope.opaque_payload_digest.as_deref() != Some(binding.as_str())
        || required(value, "receipt_digest")? != envelope.receipt_digest
    {
        bail!("ACCEPTANCE_RECEIPT_BINDING_MISMATCH");
    }
    let artifact_ref = required(value, "artifact_ref")?.to_string();
    if artifact_ref != artifact_digest {
        bail!("ARTIFACT_REF_DIGEST_MISMATCH");
    }
    Ok(AcceptedCandidate {
        issuer: envelope.issuer,
        receipt_digest: envelope.receipt_digest,
        candidate_id: required(value, "candidate_id")?.into(),
        candidate_digest,
        artifact_ref,
        artifact_digest,
        manifest_ref: required(value, "manifest_ref")?.into(),
        manifest_digest,
        evidence_digest,
    })
}

fn validate_source_binding(
    request: &DevelopmentRequest,
    run: &Run,
    session: &Session,
    source_message_id: &str,
) -> Result<()> {
    if source_message_id.trim().is_empty()
        || request.source_message_id != source_message_id
        || request.source_subject != run.principal.principal_id.0
        || request.source_scope != session.id.0
        || request.idempotency_key != format!("development:{source_message_id}")
    {
        bail!("DEVELOPMENT_REQUEST_SOURCE_BINDING_MISMATCH");
    }
    Ok(())
}

pub(super) fn validate_private_owner_context(
    configured_owner: Option<&str>,
    run: &Run,
    session: &Session,
) -> Result<()> {
    let owner = configured_owner
        .filter(|owner| !owner.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("CODING_OWNER_NOT_CONFIGURED"))?;
    let expected_principal = format!("feishu:open_id:{owner}");
    if !matches!(session.channel, ChannelKind::Feishu)
        || !matches!(run.principal.source, PrincipalSource::Feishu)
        || !matches!(&run.principal.subject, PrincipalSubject::FeishuOpenId(id) if id == owner)
        || run.principal.principal_id.0 != expected_principal
        || session.conversation_key != expected_principal
    {
        bail!("CODING_REQUIRES_OWNER_PRIVATE_FEISHU_SESSION");
    }
    Ok(())
}

fn digest_json(value: &impl serde::Serialize) -> Result<String> {
    Ok(Sha256Digest::compute(&serde_json::to_vec(value)?)
        .as_str()
        .to_string())
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("MISSING_{key}"))
}

fn digest(value: &Value, key: &str) -> Result<String> {
    let value = required(value, key)?;
    Sha256Digest::parse(value)?;
    Ok(value.into())
}
