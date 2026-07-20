use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::config::KernelConfig;
use crate::contract_catalog::ContractCatalog;
use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::{CodingTaskSubmissionClaim, JournalStore};
use crate::server::{coding_harness_client, hcr_acceptance};
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct CodingTaskSubmitResult {
    pub development_request_id: String,
    pub contract_catalog_version: String,
    pub component_profile: String,
    pub submit_invocation_id: String,
    pub acceptance_invocation_id: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub harness_execution_id: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub artifact_ref: String,
    pub artifact_digest: String,
    pub evidence_digest: String,
    pub settlement_id: String,
    pub proposal_id: String,
}

/// Execute the real submit → candidate → HCR acceptance → Proposal chain.
pub fn handle_coding_task_submit(
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    request: &DevelopmentRequest,
    run: &Run,
    session: &Session,
    source_message_id: &str,
) -> Result<CodingTaskSubmitResult> {
    ContractCatalog::v1().validate_request(request)?;
    validate_private_owner_context(config.feishu_coding_owner_id.as_deref(), run, session)?;
    if source_message_id.trim().is_empty() {
        bail!("MISSING_SOURCE_MESSAGE_ID");
    }
    if request.source_message_id != source_message_id
        || request.source_subject != run.principal.principal_id.0
        || request.source_scope != session.id.0
        || request.idempotency_key != format!("development:{source_message_id}")
    {
        bail!("DEVELOPMENT_REQUEST_SOURCE_BINDING_MISMATCH");
    }
    let snapshot = journal.load_registry_snapshot(&run.registry_snapshot_id)?;

    // 1. Claim durable ownership before invoking the Harness. Concurrent
    // delivery of one message either observes this claim or the stored result.
    let submit_key = request.idempotency_key.clone();
    let request_identity = serde_json::to_value(request)?;
    let request_digest = Sha256Digest::compute(&serde_json::to_vec(&request_identity)?)
        .as_str()
        .to_string();
    let proposed_invocation = InvocationId::new();
    let claim = journal.claim_coding_task_submission(
        source_message_id,
        &request_digest,
        &proposed_invocation,
        &run.id,
        &session.id,
    )?;
    let (submit_invocation, submitted) = match claim {
        CodingTaskSubmissionClaim::InProgress => bail!("CODING_TASK_ALREADY_IN_PROGRESS"),
        CodingTaskSubmissionClaim::Succeeded {
            invocation_id,
            result,
        } => {
            let submitted = validate_submit_result(&result, request)?;
            (invocation_id, submitted)
        }
        CodingTaskSubmissionClaim::Claimed { invocation_id } => {
            match execute_new_submission(
                journal,
                gateway,
                config,
                run,
                session,
                &snapshot,
                &invocation_id,
                &submit_key,
                request,
            ) {
                Ok((result, submitted)) => {
                    journal.complete_coding_task_submission(
                        source_message_id,
                        &invocation_id,
                        &result,
                    )?;
                    (invocation_id, submitted)
                }
                Err(error) => {
                    journal.fail_coding_task_submission(
                        source_message_id,
                        &invocation_id,
                        "SUBMIT_FAILED",
                    )?;
                    return Err(error);
                }
            }
        }
    };

    // 2. Only a successfully created Harness candidate may create an HCR.
    let requirement = json!({
        "development_request": request,
        "submit_invocation_id": submit_invocation.0,
        "candidate_ref": submitted.candidate_ref,
        "candidate_id": submitted.candidate_id,
        "candidate_digest": submitted.candidate_digest,
    });
    let (hcr_id, deduplicated) = journal.create_harness_change_request(
        "CodingRouter",
        source_message_id,
        &session.id.0,
        &run.principal.principal_id.0,
        channel_name(session.channel.clone()),
        chat_type_name(session.channel.clone()),
        "coding-harness-v0",
        &requirement.to_string(),
    )?;
    if deduplicated {
        if let Some(existing) =
            load_existing_result(journal, &hcr_id, &submit_invocation.0, request)?
        {
            return Ok(existing);
        }
        bail!("CODING_TASK_ALREADY_IN_PROGRESS");
    }

    // 3. Existing PR2 acceptance performs its own Registry/Gateway approval,
    // invokes the real Harness and persists five attempts/evidence + Receipt.
    let accepted = hcr_acceptance::handle(
        journal,
        gateway,
        config,
        &hcr_id,
        &json!({"candidate_ref": submitted.candidate_ref}),
    )?;
    let accepted_digest = required_str(&accepted, "candidate_digest")?;
    if accepted_digest != submitted.candidate_digest {
        bail!("CANDIDATE_DIGEST_CHANGED_BETWEEN_SUBMIT_AND_ACCEPTANCE");
    }
    match required_str(&accepted, "outcome")? {
        "CandidatePassed" => {}
        "CandidateFailed" => bail!("CANDIDATE_NOT_ACCEPTED"),
        "InfrastructureFailure" => bail!("CODING_ACCEPTANCE_INFRASTRUCTURE_FAILURE"),
        _ => bail!("CODING_ACCEPTANCE_OUTCOME_INVALID"),
    }

    let candidate_id = required_str(&accepted, "candidate_id")?.to_string();
    let artifact_ref = required_str(&accepted, "artifact_ref")?.to_string();
    let artifact_digest = required_digest(&accepted, "artifact_digest")?;
    let evidence_digest = required_digest(&accepted, "evidence_digest")?;
    let settlement_id = required_str(&accepted, "settlement_id")?.to_string();
    let claim_id = required_str(&accepted, "claim_id")?.to_string();
    let hcr_run_id = required_str(&accepted, "run_id")?.to_string();
    let harness_execution_id = required_str(&accepted, "harness_execution_id")?.to_string();
    let acceptance_invocation_id = required_str(&accepted, "acceptance_invocation_id")?.to_string();

    // 4. Artifact and evidence were stored by the Harness. Kernel re-loads
    // and hashes both, then builds (or loads) the activation manifest.
    let store = ContentStore::new(config.harness_artifact_root.clone());
    let artifact_key = Sha256Digest::parse(&artifact_digest)?;
    let evidence_key = Sha256Digest::parse(&evidence_digest)?;
    store.load(&artifact_key)?;
    store.load(&evidence_key)?;

    // 4b. Unified delivery manifest path — Kernel never branches on
    //     target_kind.  The Coding Harness constructs both service and
    //     invocable delivery manifests during acceptance.  Kernel loads
    //     the content‑addressed bytes, verifies the digest, and uses
    //     the exact same ref/digest for the Proposal — without parsing
    //     or understanding the manifest type.
    let delivery_ref = required_str(&accepted, "delivery_manifest_ref")?.to_string();
    let delivery_digest_str = required_digest(&accepted, "delivery_manifest_digest")?;
    let delivery_digest_key = Sha256Digest::parse(&delivery_digest_str)?;
    let bytes = store.load(&delivery_digest_key)?;
    let computed = Sha256Digest::compute(&bytes);
    if computed.as_str() != delivery_digest_str {
        bail!("DELIVERY_MANIFEST_DIGEST_TAMPERED");
    }
    let manifest_ref = delivery_ref;
    let manifest_bytes = bytes.to_vec();
    let manifest_digest = store.store(&manifest_bytes)?.as_str().to_string();

    let proposal_id = format!("proposal_{}", uuid::Uuid::new_v4().simple());
    let proposal = CapabilityChangeProposal::new(
        proposal_id.clone(),
        run.principal.principal_id.0.clone(),
        run.agent_id.clone(),
        session.id.clone(),
        run.id.clone(),
        artifact_ref.clone(),
        artifact_digest.clone(),
        manifest_ref,
        manifest_digest,
        evidence_digest.clone(),
        evidence_digest.clone(),
        vec![request.name.clone()],
        format!(
            "{}; profile {}; five sandboxed gates passed",
            request.request_id, request.build_profile
        ),
        run.registry_snapshot_id.clone(),
    );
    let link = CapabilityProposalHcrLink {
        proposal_id: proposal_id.clone(),
        hcr_id: hcr_id.clone(),
        claim_id: claim_id.clone(),
        run_id: hcr_run_id.clone(),
        operation: request.name.clone(),
        candidate_id: candidate_id.clone(),
        candidate_digest: accepted_digest.to_string(),
        artifact_ref: artifact_ref.clone(),
        artifact_digest: artifact_digest.clone(),
        evidence_digest: evidence_digest.clone(),
        source_registry_snapshot_id: run.registry_snapshot_id.clone(),
        settlement_id: settlement_id.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let proposal_id = journal.create_proposal_with_hcr_link(&proposal, &link)?;

    Ok(CodingTaskSubmitResult {
        development_request_id: request.request_id.clone(),
        contract_catalog_version: request.contract_catalog_version.clone(),
        component_profile: request.build_profile.clone(),
        submit_invocation_id: submit_invocation.0,
        acceptance_invocation_id,
        hcr_id,
        claim_id,
        run_id: hcr_run_id,
        harness_execution_id,
        candidate_id,
        candidate_digest: accepted_digest.to_string(),
        artifact_ref,
        artifact_digest,
        evidence_digest,
        settlement_id,
        proposal_id,
    })
}

fn channel_name(channel: ChannelKind) -> &'static str {
    match channel {
        ChannelKind::Feishu => "Feishu",
        ChannelKind::Cli => "Cli",
    }
}

fn chat_type_name(channel: ChannelKind) -> &'static str {
    match channel {
        ChannelKind::Feishu => "p2p",
        ChannelKind::Cli => "cli",
    }
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
) -> Result<(Value, SubmittedCandidate)> {
    let submit_intent = InvocationIntent {
        invocation_id: invocation_id.clone(),
        run_id: run.id.clone(),
        operation: crate::domain::operation::external::TASK_SUBMIT.to_string(),
        arguments: json!({
            "session_id": session.id.0,
            "development_request": request,
            "idempotency_key": submit_key,
        }),
        idempotency_key: Some(submit_key.to_string()),
    };
    super::invocation_journal::append_invocation_proposed(journal, run, session, &submit_intent)?;
    let approved = gateway.approve_invocation(submit_intent, run, session, snapshot)?;
    super::invocation_journal::append_invocation_approved(journal, run, session, &approved)?;
    let result = coding_harness_client::execute(
        &approved,
        std::time::Duration::from_millis(config.harness_read_timeout_ms.max(900_000)),
    )?;
    let submitted = validate_submit_result(&result, request)?;
    journal.append_event(
        JournalEventKind::ReceiptReceived,
        Some(&run.id),
        Some(&session.id),
        Some(&invocation_id.0),
        json!({
            "invocation_id": invocation_id.0,
            "operation": crate::domain::operation::external::TASK_SUBMIT,
            "status": "Succeeded",
            "output": result,
        }),
    )?;
    Ok((result, submitted))
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("MISSING_{key}"))
}

fn required_digest(value: &Value, key: &str) -> Result<String> {
    let value = required_str(value, key)?;
    Sha256Digest::parse(value)?;
    Ok(value.to_string())
}

struct SubmittedCandidate {
    candidate_id: String,
    candidate_ref: String,
    candidate_digest: String,
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

fn validate_submit_result(
    value: &Value,
    request: &DevelopmentRequest,
) -> Result<SubmittedCandidate> {
    if required_str(value, "request_id")? != request.request_id {
        bail!("HARNESS_DEVELOPMENT_REQUEST_ID_MISMATCH");
    }
    let candidate_id = required_str(value, "candidate_id")?.to_string();
    let candidate_ref = required_str(value, "candidate_ref")?.to_string();
    if !candidate_ref.starts_with("generated/")
        || candidate_ref.contains("..")
        || std::path::Path::new(&candidate_ref).is_absolute()
    {
        bail!("INVALID_CANDIDATE_REF");
    }
    let candidate_digest = required_digest(value, "candidate_digest")?;
    Ok(SubmittedCandidate {
        candidate_id,
        candidate_ref,
        candidate_digest,
    })
}

fn load_existing_result(
    journal: &JournalStore,
    hcr_id: &str,
    submit_invocation_id: &str,
    request: &DevelopmentRequest,
) -> Result<Option<CodingTaskSubmitResult>> {
    let Some(link) = journal.load_proposal_hcr_link_by_hcr(hcr_id)? else {
        return Ok(None);
    };
    let Some((acceptance_invocation_id, harness_execution_id)) =
        journal.load_hcr_receipt_identity(hcr_id)?
    else {
        return Ok(None);
    };
    Ok(Some(CodingTaskSubmitResult {
        development_request_id: request.request_id.clone(),
        contract_catalog_version: request.contract_catalog_version.clone(),
        component_profile: request.build_profile.clone(),
        submit_invocation_id: submit_invocation_id.to_string(),
        acceptance_invocation_id,
        hcr_id: hcr_id.to_string(),
        claim_id: link.claim_id,
        run_id: link.run_id,
        harness_execution_id,
        candidate_id: link.candidate_id,
        candidate_digest: link.candidate_digest,
        artifact_ref: link.artifact_ref,
        artifact_digest: link.artifact_digest,
        evidence_digest: link.evidence_digest,
        settlement_id: link.settlement_id,
        proposal_id: link.proposal_id,
    }))
}
