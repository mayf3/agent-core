//! Trusted orchestration for the fixed North Star coding task.

use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::config::KernelConfig;
use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::{CodingTaskSubmissionClaim, JournalStore};
use crate::server::{coding_harness_client, coding_router::CodingIntent, hcr_acceptance};
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CodingTaskSubmitResult {
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
    intent: &CodingIntent,
    run: &Run,
    session: &Session,
    source_message_id: &str,
) -> Result<CodingTaskSubmitResult> {
    validate_fixed_intent(intent)?;
    validate_private_owner_context(config.feishu_coding_owner_id.as_deref(), run, session)?;
    if source_message_id.trim().is_empty() {
        bail!("MISSING_SOURCE_MESSAGE_ID");
    }
    let snapshot = journal.load_registry_snapshot(&run.registry_snapshot_id)?;

    // 1. Claim durable ownership before invoking the Harness. Concurrent
    // delivery of one message either observes this claim or the stored result.
    let submit_key = format!("calculator-submit:{source_message_id}");
    let request_identity = json!({
        "session_id": session.id.0,
        "principal_id": run.principal.principal_id.0,
        "kind": "DevelopCapability",
        "operation": intent.operation,
        "functions": intent.functions,
        "schema_version": intent.schema_version,
    });
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
            let submitted = validate_submit_result(&result)?;
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
                intent,
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
        "kind": "DevelopCapability",
        "operation": intent.operation,
        "functions": intent.functions,
        "schema_version": intent.schema_version,
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
        if let Some(existing) = load_existing_result(journal, &hcr_id, &submit_invocation.0)? {
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
    if required_str(&accepted, "outcome")? != "CandidatePassed" {
        bail!("CANDIDATE_NOT_ACCEPTED");
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

    // 4. Artifact and evidence were stored by the Harness.  Kernel re-loads
    // and hashes both, then builds a real activation manifest in the same CAS.
    let store = ContentStore::new(config.harness_artifact_root.clone());
    let artifact_key = Sha256Digest::parse(&artifact_digest)?;
    let evidence_key = Sha256Digest::parse(&evidence_digest)?;
    store.load(&artifact_key)?;
    store.load(&evidence_key)?;
    let manifest = calculator_manifest(&artifact_digest)?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
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
        manifest.manifest_id.clone(),
        manifest_digest,
        evidence_digest.clone(),
        evidence_digest.clone(),
        vec!["external.calculator".to_string()],
        "fixed calculator-v0; five Linux bubblewrap gates passed".to_string(),
        run.registry_snapshot_id.clone(),
    );
    let link = CapabilityProposalHcrLink {
        proposal_id: proposal_id.clone(),
        hcr_id: hcr_id.clone(),
        claim_id: claim_id.clone(),
        run_id: hcr_run_id.clone(),
        operation: "external.calculator".to_string(),
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
    intent: &CodingIntent,
) -> Result<(Value, SubmittedCandidate)> {
    let submit_intent = InvocationIntent {
        invocation_id: invocation_id.clone(),
        run_id: run.id.clone(),
        operation: crate::domain::operation::external::TASK_SUBMIT.to_string(),
        arguments: json!({
            "session_id": session.id.0,
            "kind": "DevelopCapability",
            "operation": intent.operation,
            "functions": intent.functions,
            "schema_version": intent.schema_version,
            "idempotency_key": submit_key,
        }),
        idempotency_key: Some(submit_key.to_string()),
    };
    append_invocation_proposed(journal, run, session, &submit_intent)?;
    let approved = gateway.approve_invocation(submit_intent, run, session, snapshot)?;
    append_invocation_approved(journal, run, session, &approved)?;
    let result = coding_harness_client::execute(
        &approved,
        Duration::from_millis(config.harness_read_timeout_ms.max(30_000)),
    )?;
    let submitted = validate_submit_result(&result)?;
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

struct SubmittedCandidate {
    candidate_id: String,
    candidate_ref: String,
    candidate_digest: String,
}

fn validate_fixed_intent(intent: &CodingIntent) -> Result<()> {
    if intent.operation != "external.calculator"
        || intent.schema_version != "calculator-v0"
        || intent.functions != ["add", "subtract", "multiply", "divide"]
    {
        bail!("UNSUPPORTED_CODING_SPEC");
    }
    Ok(())
}

fn validate_private_owner_context(
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

#[cfg(test)]
#[path = "coding_private_origin_tests.rs"]
mod private_origin_tests;

fn validate_submit_result(value: &Value) -> Result<SubmittedCandidate> {
    if required_str(value, "operation")? != "external.calculator"
        || required_str(value, "schema_version")? != "calculator-v0"
    {
        bail!("HARNESS_SUBMIT_IDENTITY_MISMATCH");
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

fn calculator_manifest(artifact_digest: &str) -> Result<HarnessManifest> {
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "capability-host-v0".to_string(),
        artifact_digest: artifact_digest.to_string(),
        protocol_version: "external-harness-v1".to_string(),
        endpoint: "http://127.0.0.1:7300/execute".to_string(),
        operation_name: "external.calculator".to_string(),
        description: "Approved calculator supporting add, subtract, multiply, and divide."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string", "enum": ["add", "subtract", "multiply", "divide"]},
                "a": {"type": "number"},
                "b": {"type": "number"}
            },
            "required": ["operation", "a", "b"],
            "additionalProperties": false
        }),
        output_schema: json!({"type": "number"}),
        idempotent: true,
        created_at: Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    manifest.validate_all()?;
    Ok(manifest)
}

fn append_invocation_proposed(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    intent: &InvocationIntent,
) -> Result<()> {
    journal.append_event(
        JournalEventKind::InvocationProposed,
        Some(&run.id),
        Some(&session.id),
        Some(&intent.invocation_id.0),
        json!({
            "invocation_id": intent.invocation_id.0,
            "operation": intent.operation,
            "idempotency_key": intent.idempotency_key,
        }),
    )?;
    Ok(())
}

fn append_invocation_approved(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    approved: &ApprovedInvocation,
) -> Result<()> {
    journal.append_event(
        JournalEventKind::InvocationApproved,
        Some(&run.id),
        Some(&session.id),
        Some(&approved.intent().invocation_id.0),
        json!({
            "invocation_id": approved.intent().invocation_id.0,
            "operation": approved.intent().operation,
            "decision_id": approved.decision_id,
        }),
    )?;
    Ok(())
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

fn load_existing_result(
    journal: &JournalStore,
    hcr_id: &str,
    submit_invocation_id: &str,
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
