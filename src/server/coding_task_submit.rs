//! Trusted orchestration for catalogued Generic DevelopmentRequests.
//!
//! # LEGACY: Managed-Service Version Allocation
//!
//! The version-resolution logic in this module (`resolve_next_deployment_version`,
//! `query_deployment_harness_version`) is a temporary bridge for the Milestone 1
//! canary path. It queries the Deployment Harness read-only status endpoint to
//! derive the next patch version for a managed service component.
//!
//! In the target architecture (External Controller / Harness), version selection
//! is the responsibility of the external development pipeline, NOT the Kernel.
//! This code MUST be removed when the external orchestration seam takes over
//! manifest construction.

use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::config::KernelConfig;
use crate::contract_catalog::ContractCatalog;
use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::{CodingTaskSubmissionClaim, JournalStore};
use crate::server::{coding_harness_client, hcr_acceptance};
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

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
    let component_manifest_digest = required_digest(&accepted, "component_manifest_digest")?;
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
    let component_manifest_key = Sha256Digest::parse(&component_manifest_digest)?;
    store.load(&artifact_key)?;
    store.load(&evidence_key)?;
    let mut component_manifest: Value = serde_json::from_slice(&store.load(&component_manifest_key)?)?;
    let (manifest_ref, manifest_bytes) = match request.target_kind {
        TargetKind::InvocableCapability => {
            let manifest = invocable_manifest(request, &component_manifest, &artifact_digest)?;
            (manifest.manifest_id.clone(), serde_json::to_vec(&manifest)?)
        }
        TargetKind::HookConsumerService => {
            // LEGACY: Resolve next deployment version from the Deployment
            // Harness. This ensures monotonically increasing patch versions.
            // See module-level doc comment for removal plan.
            let component_id = request.name.clone();
            let override_version = resolve_next_deployment_version(&component_id)?;
            if let (Some(ver), Some(service)) = (&override_version, component_manifest.get_mut("service").and_then(|v| v.as_object_mut())) {
                service.insert("version".into(), json!(ver));
            }
            let manifest = service_manifest(request, &component_manifest, &artifact_digest)?;
            (manifest.manifest_id.clone(), serde_json::to_vec(&manifest)?)
        }
        _ => bail!("DEPLOYMENT_PROFILE_NOT_IMPLEMENTED"),
    };
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
    append_invocation_proposed(journal, run, session, &submit_intent)?;
    let approved = gateway.approve_invocation(submit_intent, run, session, snapshot)?;
    append_invocation_approved(journal, run, session, &approved)?;
    let result = coding_harness_client::execute(
        &approved,
        Duration::from_millis(config.harness_read_timeout_ms.max(900_000)),
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

struct SubmittedCandidate {
    candidate_id: String,
    candidate_ref: String,
    candidate_digest: String,
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

fn invocable_manifest(
    request: &DevelopmentRequest,
    component: &Value,
    artifact_digest: &str,
) -> Result<HarnessManifest> {
    if request.target_kind != TargetKind::InvocableCapability
        || required_str(component, "schema_version")? != "component-artifact-v1"
        || required_str(component, "kind")? != "invocable_capability"
        || required_str(component, "component_id")? != request.name
        || required_str(component, "profile_id")? != request.build_profile
        || required_str(component, "contract_catalog_version")? != request.contract_catalog_version
        || required_str(component, "deployment_profile")? != request.deployment_profile
        || !string_set_matches(component, "required_contracts", &request.required_contracts)?
        || !string_set_matches(
            component,
            "requested_permissions",
            &request.requested_permissions,
        )?
    {
        bail!("COMPONENT_MANIFEST_IDENTITY_MISMATCH");
    }
    let capability = component
        .get("capability")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow::anyhow!("CAPABILITY_MANIFEST_MISSING"))?;
    if required_str(capability, "operation_name")? != request.name {
        bail!("CAPABILITY_OPERATION_MISMATCH");
    }
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "capability-host-v0".to_string(),
        artifact_digest: artifact_digest.to_string(),
        protocol_version: "external-harness-v1".to_string(),
        endpoint: "http://127.0.0.1:7300/execute".to_string(),
        operation_name: request.name.clone(),
        description: required_str(capability, "description")?.to_string(),
        input_schema: capability
            .get("input_schema")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_INPUT_SCHEMA_MISSING"))?,
        output_schema: capability
            .get("output_schema")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_OUTPUT_SCHEMA_MISSING"))?,
        idempotent: capability
            .get("idempotent")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_IDEMPOTENCY_MISSING"))?,
        created_at: Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    manifest.validate_all()?;
    Ok(manifest)
}

fn service_manifest(
    request: &DevelopmentRequest,
    component: &Value,
    artifact_digest: &str,
) -> Result<ServiceManifest> {
    if request.target_kind != TargetKind::HookConsumerService
        || required_str(component, "schema_version")? != "component-artifact-v1"
        || required_str(component, "kind")? != "hook_consumer_service"
        || required_str(component, "component_id")? != request.name
        || required_str(component, "profile_id")? != request.build_profile
        || required_str(component, "contract_catalog_version")? != request.contract_catalog_version
        || required_str(component, "deployment_profile")? != request.deployment_profile
        || !string_set_matches(component, "required_contracts", &request.required_contracts)?
        || !string_set_matches(
            component,
            "requested_permissions",
            &request.requested_permissions,
        )?
    {
        bail!("COMPONENT_MANIFEST_IDENTITY_MISMATCH");
    }
    let service = component
        .get("service")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow::anyhow!("SERVICE_COMPONENT_MANIFEST_MISSING"))?;
    let mut manifest = ServiceManifest {
        schema_version: SERVICE_MANIFEST_SCHEMA.into(),
        manifest_id: String::new(),
        component_id: request.name.clone(),
        kind: TargetKind::HookConsumerService,
        artifact_digest: artifact_digest.into(),
        entrypoint: "artifact".into(),
        runtime_profile: request.deployment_profile.clone(),
        version: required_str(service, "version")?.into(),
        required_contracts: request.required_contracts.clone(),
        requested_permissions: request.requested_permissions.clone(),
        listen_policy: ListenPolicy {
            host: "127.0.0.1".into(),
            port: 0,
            exposure: "loopback".into(),
        },
        healthcheck: ServiceHealthcheck {
            method: "GET".into(),
            path: required_str(service, "healthcheck_path")?.into(),
            timeout_ms: 10_000,
        },
        state_path: "state".into(),
        upgrade_policy: UpgradePolicy {
            strategy: "replace_after_ready".into(),
            require_healthy_before_switch: true,
        },
        rollback_policy: RollbackPolicy {
            retain_previous_versions: 2,
            automatic_on_health_failure: true,
        },
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    manifest.validate()?;
    Ok(manifest)
}

fn string_set_matches(value: &Value, key: &str, expected: &[String]) -> Result<bool> {
    let values = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MISSING_{key}"))?;
    let actual = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("INVALID_{key}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let actual_set: std::collections::HashSet<_> = actual.iter().collect();
    let expected_set: std::collections::HashSet<_> = expected.iter().collect();
    Ok(actual.len() == expected.len() && actual_set == expected_set)
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

// ---------------------------------------------------------------------------
// LEGACY: Managed-Service Version Allocation
//
// Queries the Deployment Harness read-only status endpoint for the current
// highest deployed version of a component, then increments the patch number.
//
// This is a temporary bridge for Milestone 1. See module-level doc comment
// for the removal plan.
// ---------------------------------------------------------------------------

/// Resolve the next deployment version for a managed-service component.
///
/// Queries the Deployment Harness for the current version of `component_id`.
/// If the component exists and has a parsable semver, returns the next patch
/// version. If the component does not exist or the version cannot be parsed,
/// returns `None` (caller falls back to the initial version from the manifest).
fn resolve_next_deployment_version(component_id: &str) -> Result<Option<String>> {
    let response = query_deployment_harness_version(component_id)?;
    match response {
        Some(current) => {
            let next = increment_patch(&current)
                .ok_or_else(|| anyhow::anyhow!("INVALID_EXISTING_VERSION: {current}"))?;
            Ok(Some(next))
        }
        None => Ok(None),
    }
}

/// Query the Deployment Harness `GET /v1/components/{component_id}` and
/// return the current deployed `version` string, if the component exists.
///
/// Returns `Ok(None)` when the component is not found (404 or `ok: false`).
/// Returns `Err` on transport errors or malformed responses.
fn query_deployment_harness_version(component_id: &str) -> Result<Option<String>> {
    let endpoint = std::env::var("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7400".into());
    let token = std::env::var("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_TOKEN")
        .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_CONTROL_NOT_CONFIGURED"))?;
    if token.len() < 32 {
        bail!("DEPLOYMENT_HARNESS_CONTROL_TOKEN_INVALID");
    }

    let authority = endpoint
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("DEPLOYMENT_HARNESS_ENDPOINT_INVALID"))?
        .trim_end_matches('/');
    let path = format!("/v1/components/{component_id}");
    let addr = authority
        .parse::<std::net::SocketAddr>()
        .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_ENDPOINT_INVALID"))?;
    if !addr.ip().is_loopback() {
        bail!("DEPLOYMENT_HARNESS_ENDPOINT_NOT_LOOPBACK");
    }

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_TCP_CONNECTION_REFUSED"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok();

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_WRITE_FAILED"))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_READ_FAILED"))?;

    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let payload = response
        .split_once("\r\n\r\n")
        .map(|(_, p)| p)
        .unwrap_or("");

    // 200 → component exists; parse version.
    if status == 200 {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(payload) {
            if val.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                let version = val
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                return Ok(version);
            }
        }
    }
    Ok(None)
}

/// Increment the patch component of a semver string "X.Y.Z".
///
/// Returns `None` if `current` is not a valid three-part semver or if any
/// component is not a non-negative integer. Overflow wraps u64.
fn increment_patch(current: &str) -> Option<String> {
    let parts: Vec<&str> = current.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let major: u64 = parts[0].parse().ok()?;
    let minor: u64 = parts[1].parse().ok()?;
    let patch: u64 = parts[2].parse().ok()?;
    Some(format!("{major}.{minor}.{}", patch.wrapping_add(1)))
}

#[cfg(test)]
mod version_resolution_tests {
    use super::*;
    use crate::contract_catalog::CONTRACT_CATALOG_VERSION;

    #[test]
    fn existing_0_1_0_increments_to_0_1_1() {
        assert_eq!(increment_patch("0.1.0"), Some("0.1.1".into()));
    }

    #[test]
    fn existing_0_1_9_increments_to_0_1_10() {
        assert_eq!(increment_patch("0.1.9"), Some("0.1.10".into()));
    }

    #[test]
    fn existing_1_0_0_increments_to_1_0_1() {
        assert_eq!(increment_patch("1.0.0"), Some("1.0.1".into()));
    }

    #[test]
    fn invalid_version_returns_none() {
        assert_eq!(increment_patch("0.1"), None);
        assert_eq!(increment_patch("0.a.0"), None);
        assert_eq!(increment_patch(""), None);
        assert_eq!(increment_patch("0.1.0.0"), None);
    }

    #[test]
    fn equal_version_is_not_reused() {
        let next = increment_patch("0.1.0").unwrap();
        assert_ne!(next, "0.1.0");
    }

    #[test]
    fn version_is_bound_into_manifest_digest() {
        let mut base = json!({
            "schema_version":"component-artifact-v1",
            "component_id":"token-dashboard",
            "kind":"hook_consumer_service",
            "profile_id":"hook-consumer-service-v0",
            "contract_catalog_version": CONTRACT_CATALOG_VERSION,
            "required_contracts":["event.observe.v0"],
            "requested_permissions":["journal.observe"],
            "deployment_profile":"managed-service-v0",
            "service":{"version":"0.1.0","healthcheck_path":"/health"}
        });
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::HookConsumerService,
            "token-dashboard".into(),
        );
        draft.build_profile = "hook-consumer-service-v0".into();
        draft.deployment_profile = "managed-service-v0".into();
        draft.required_contracts = vec!["event.observe.v0".into()];
        draft.requested_permissions = vec!["journal.observe".into()];
        draft.requirements = vec!["test requirement".into()];
        draft.acceptance_criteria = vec!["test acceptance".into()];
        let request = DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "session:test".into(),
            "message:service".into(),
            "development:message:service".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap();

        let digest = format!("sha256:{}", "a".repeat(64));
        base["service"]["version"] = json!("0.1.0");
        let m1 = service_manifest(&request, &base, &digest).unwrap();
        base["service"]["version"] = json!("0.1.1");
        let m2 = service_manifest(&request, &base, &digest).unwrap();
        assert_ne!(m1.manifest_id, m2.manifest_id,
            "different versions must produce different manifest_ids");
    }
}

#[cfg(test)]
mod component_manifest_tests {
    use super::*;
    use crate::contract_catalog::CONTRACT_CATALOG_VERSION;

    fn request() -> DevelopmentRequest {
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.example".into(),
        );
        draft.requirements = vec!["provide a bounded invocation".into()];
        draft.required_contracts = vec!["component.invoke.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["trusted contract tests pass".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "session:test".into(),
            "message:test".into(),
            "development:message:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    fn component() -> Value {
        json!({
            "schema_version": "component-artifact-v1",
            "component_id": "external.example",
            "kind": "invocable_capability",
            "profile_id": "invocable-capability-v0",
            "contract_catalog_version": CONTRACT_CATALOG_VERSION,
            "required_contracts": ["component.invoke.v0"],
            "requested_permissions": ["component.invoke"],
            "deployment_profile": "capability-host-v0",
            "capability": {
                "operation_name": "external.example",
                "description": "A bounded example capability.",
                "input_schema": {"type":"object","additionalProperties":false},
                "output_schema": {"type":"object"},
                "idempotent": true
            }
        })
    }

    fn service_request() -> DevelopmentRequest {
        let mut draft =
            DevelopmentRequestDraft::new(TargetKind::HookConsumerService, "token-dashboard".into());
        draft.requirements = vec!["consume durable model usage facts".into()];
        draft.required_contracts = vec!["event.observe.v0".into()];
        draft.requested_permissions = vec!["journal.observe".into()];
        draft.acceptance_criteria = vec!["projection is rebuildable".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "session:test".into(),
            "message:service".into(),
            "development:message:service".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    fn service_component() -> Value {
        json!({
            "schema_version":"component-artifact-v1",
            "component_id":"token-dashboard",
            "kind":"hook_consumer_service",
            "profile_id":"hook-consumer-service-v0",
            "contract_catalog_version":CONTRACT_CATALOG_VERSION,
            "required_contracts":["event.observe.v0"],
            "requested_permissions":["journal.observe"],
            "deployment_profile":"managed-service-v0",
            "service":{"version":"0.1.0","healthcheck_path":"/health"}
        })
    }

    #[test]
    fn post_gate_manifest_must_match_requested_contracts_and_permissions() {
        let request = request();
        let digest = format!("sha256:{}", "a".repeat(64));
        invocable_manifest(&request, &component(), &digest).unwrap();

        let mut escalated = component();
        escalated["requested_permissions"] = json!(["component.invoke", "deployment.effect"]);
        assert!(invocable_manifest(&request, &escalated, &digest).is_err());
    }

    #[test]
    fn hook_consumer_manifest_becomes_a_restricted_service_contract() {
        let request = service_request();
        let digest = format!("sha256:{}", "b".repeat(64));
        let manifest = service_manifest(&request, &service_component(), &digest).unwrap();
        assert_eq!(manifest.component_id, "token-dashboard");
        assert_eq!(manifest.entrypoint, "artifact");
        assert_eq!(manifest.listen_policy.host, "127.0.0.1");
        assert_eq!(manifest.listen_policy.port, 0);
        assert_eq!(manifest.requested_permissions, ["journal.observe"]);

        let mut escalated = service_component();
        escalated["requested_permissions"] = json!(["journal.observe", "kernel.write"]);
        assert!(service_manifest(&request, &escalated, &digest).is_err());
    }
}
