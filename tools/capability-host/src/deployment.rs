//! Durable deployment bindings for governed invocable capabilities.

use crate::artifact::{resolve_artifact, ArtifactError, ResolvedArtifact};
use crate::config::CapabilityHostConfig;
use crate::process::run_artifact;
use crate::protocol::{self, HarnessRequest};
use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::harness::manifest::HarnessManifest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

mod state;

const DESCRIBE_OPERATION: &str = "__agent_core_describe";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub protocol_version: String,
    pub proposal_id: String,
    pub decision_id: String,
    pub manifest_digest: String,
    pub artifact_digest: String,
    pub target_registry_snapshot_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamRead {
    pub component_id: String,
    pub method: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionDescriptor {
    pub descriptor_version: String,
    pub operation_name: String,
    pub probe_arguments: Value,
    #[serde(default)]
    pub upstream: Option<UpstreamRead>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentRecord {
    pub deployment_id: String,
    pub proposal_id: String,
    pub decision_id: String,
    pub manifest_digest: String,
    pub manifest_id: String,
    pub artifact_digest: String,
    pub operation_name: String,
    pub target_registry_snapshot_id: String,
    pub probe_execution_id: String,
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default)]
    pub output_schema: Value,
    #[serde(default)]
    pub execution: Option<ExecutionDescriptor>,
}

pub fn prepare(config: &CapabilityHostConfig, body: &str) -> Result<Value, DeployError> {
    let request: DeployRequest =
        serde_json::from_str(body).map_err(|_| DeployError::Invalid("malformed_deploy_request"))?;
    validate_identity(&request)?;
    let manifest = load_manifest(&config.artifact_root, &request.manifest_digest)?;
    validate_manifest(config, &request, &manifest)?;
    let _deployment_lock = state::lock(config)?;
    if let Some(existing) = state::load(config, &manifest.operation_name)? {
        if matches_request(&existing, &request) {
            return Ok(response(&existing, true));
        }
        return Err(DeployError::Conflict);
    }
    let artifact = resolve_artifact(&config.artifact_root, &request.artifact_digest)
        .map_err(map_artifact_error)?;
    let execution = describe(config, &artifact, &manifest)?;

    let mut record = DeploymentRecord {
        deployment_id: state::deployment_id(
            &request,
            &manifest.manifest_id,
            &manifest.operation_name,
        ),
        proposal_id: request.proposal_id,
        decision_id: request.decision_id,
        manifest_digest: request.manifest_digest,
        manifest_id: manifest.manifest_id,
        artifact_digest: request.artifact_digest,
        operation_name: manifest.operation_name,
        target_registry_snapshot_id: request.target_registry_snapshot_id,
        probe_execution_id: String::new(),
        input_schema: manifest.input_schema,
        output_schema: manifest.output_schema,
        execution: Some(execution),
    };
    let probe_arguments = record
        .execution
        .as_ref()
        .map(|value| value.probe_arguments.clone())
        .unwrap_or_else(|| json!({}));
    record.probe_execution_id = execution_id(
        &record.deployment_id,
        "capability-host-deploy-probe",
        &probe_arguments,
    );
    probe(config, &artifact, &record)?;
    state::persist(config, &record)?;
    Ok(response(&record, false))
}

pub fn authorize_execution(
    config: &CapabilityHostConfig,
    request: &HarnessRequest,
) -> Result<DeploymentRecord, DeployError> {
    let record = state::load(config, &request.operation_name)?.ok_or(DeployError::NotDeployed)?;
    if request.operation_name != record.operation_name
        || request.manifest_id != record.manifest_id
        || request.artifact_digest != record.artifact_digest
        || request.registry_snapshot_id != record.target_registry_snapshot_id
    {
        return Err(DeployError::BindingMismatch);
    }
    validate_user_arguments(&record.input_schema, &request.arguments)?;
    Ok(record)
}

pub fn process_arguments(
    record: &DeploymentRecord,
    user_arguments: &Value,
) -> Result<Value, DeployError> {
    let mut arguments = user_arguments
        .as_object()
        .cloned()
        .ok_or(DeployError::Invalid("invalid_capability_arguments"))?;
    if arguments.contains_key("__agent_core_upstream_state") {
        return Err(DeployError::Invalid("reserved_argument_denied"));
    }
    if let Some(upstream) = record
        .execution
        .as_ref()
        .and_then(|value| value.upstream.as_ref())
    {
        let value = crate::upstream::read(upstream).map_err(|_| DeployError::UpstreamFailed)?;
        arguments.insert("__agent_core_upstream_state".into(), value);
    }
    Ok(Value::Object(arguments))
}

pub fn validate_output(record: &DeploymentRecord, output: &Value) -> Result<(), DeployError> {
    if record.output_schema.is_null() {
        return Ok(());
    }
    agent_core_kernel::registry::schema::validate_against_schema(&record.output_schema, output)
        .map_err(|_| DeployError::Invalid("capability_output_schema_violation"))
}

pub fn execution_id(deployment_id: &str, invocation_id: &str, arguments: &Value) -> String {
    let canonical = json!({
        "deployment_id": deployment_id,
        "invocation_id": invocation_id,
        "arguments": arguments,
    });
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    format!("che_{}", hex::encode(Sha256::digest(bytes)))
}

fn validate_identity(request: &DeployRequest) -> Result<(), DeployError> {
    if request.protocol_version != "capability-deploy-v1" {
        return Err(DeployError::Invalid("unsupported_deploy_protocol"));
    }
    for value in [
        &request.proposal_id,
        &request.decision_id,
        &request.target_registry_snapshot_id,
    ] {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_whitespace) {
            return Err(DeployError::Invalid("invalid_deployment_identity"));
        }
    }
    Sha256Digest::parse(&request.manifest_digest)
        .map_err(|_| DeployError::Invalid("manifest_digest_invalid"))?;
    Sha256Digest::parse(&request.artifact_digest)
        .map_err(|_| DeployError::Invalid("artifact_digest_invalid"))?;
    Ok(())
}

fn load_manifest(root: &Path, digest: &str) -> Result<HarnessManifest, DeployError> {
    let digest =
        Sha256Digest::parse(digest).map_err(|_| DeployError::Invalid("manifest_digest_invalid"))?;
    let bytes = ContentStore::new(root.to_path_buf())
        .load(&digest)
        .map_err(|_| DeployError::Invalid("manifest_not_found_or_mismatched"))?;
    serde_json::from_slice(&bytes).map_err(|_| DeployError::Invalid("manifest_invalid"))
}

fn validate_manifest(
    config: &CapabilityHostConfig,
    request: &DeployRequest,
    manifest: &HarnessManifest,
) -> Result<(), DeployError> {
    manifest
        .validate_all()
        .map_err(|_| DeployError::Invalid("manifest_invalid"))?;
    if manifest.compute_manifest_id().ok().as_deref() != Some(&manifest.manifest_id)
        || manifest.harness_id != "capability-host-v0"
        || manifest.protocol_version != "external-harness-v1"
        || manifest.artifact_digest != request.artifact_digest
        || !safe_operation(&manifest.operation_name)
    {
        return Err(DeployError::Invalid("manifest_binding_invalid"));
    }
    let endpoint = manifest
        .parse_endpoint()
        .map_err(|_| DeployError::Invalid("manifest_endpoint_invalid"))?;
    let listen_port = config
        .listen_addr
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok());
    if endpoint.path != "/execute" || Some(endpoint.port) != listen_port {
        return Err(DeployError::Invalid("manifest_endpoint_invalid"));
    }
    Ok(())
}

fn describe(
    config: &CapabilityHostConfig,
    artifact: &ResolvedArtifact,
    manifest: &HarnessManifest,
) -> Result<ExecutionDescriptor, DeployError> {
    let request = HarnessRequest {
        protocol_version: "external-harness-v1".into(),
        operation_name: DESCRIBE_OPERATION.into(),
        invocation_id: "capability-host-describe".into(),
        arguments: json!({}),
        manifest_id: manifest.manifest_id.clone(),
        artifact_digest: manifest.artifact_digest.clone(),
        registry_snapshot_id: "capability-host-describe".into(),
    };
    let stdin = serde_json::to_string(&protocol::build_process_request(&request))
        .map_err(|_| DeployError::DescriptorInvalid)?;
    let output = run_artifact(
        artifact,
        &stdin,
        config.exec_timeout,
        config.max_stdout_bytes,
        config.max_stderr_bytes,
    )
    .map_err(|_| DeployError::DescriptorInvalid)?;
    let (ok, value) = protocol::map_process_response(&output.stdout);
    let descriptor: ExecutionDescriptor = value
        .get("result")
        .cloned()
        .filter(|_| ok && output.exit_code == Some(0))
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or(DeployError::DescriptorInvalid)?;
    validate_descriptor(&descriptor, manifest)?;
    Ok(descriptor)
}

fn validate_descriptor(
    descriptor: &ExecutionDescriptor,
    manifest: &HarnessManifest,
) -> Result<(), DeployError> {
    if descriptor.descriptor_version != "invocable-execution-v0"
        || descriptor.operation_name != manifest.operation_name
    {
        return Err(DeployError::DescriptorInvalid);
    }
    validate_user_arguments(&manifest.input_schema, &descriptor.probe_arguments)?;
    if let Some(upstream) = &descriptor.upstream {
        if upstream.method != "GET"
            || !safe_component_id(&upstream.component_id)
            || !safe_read_path(&upstream.path)
        {
            return Err(DeployError::DescriptorInvalid);
        }
    }
    Ok(())
}

fn validate_user_arguments(schema: &Value, arguments: &Value) -> Result<(), DeployError> {
    let object = arguments
        .as_object()
        .ok_or(DeployError::Invalid("invalid_capability_arguments"))?;
    if object.keys().any(|key| key.starts_with("__agent_core_")) {
        return Err(DeployError::Invalid("reserved_argument_denied"));
    }
    if schema.is_null() {
        return Ok(());
    }
    agent_core_kernel::registry::schema::validate_against_schema(schema, arguments)
        .map_err(|_| DeployError::Invalid("capability_input_schema_violation"))
}

fn probe(
    config: &CapabilityHostConfig,
    artifact: &ResolvedArtifact,
    record: &DeploymentRecord,
) -> Result<(), DeployError> {
    let user_arguments = record
        .execution
        .as_ref()
        .map(|value| value.probe_arguments.clone())
        .ok_or(DeployError::DescriptorInvalid)?;
    let arguments = process_arguments(record, &user_arguments)?;
    let request = HarnessRequest {
        protocol_version: "external-harness-v1".into(),
        operation_name: record.operation_name.clone(),
        invocation_id: "capability-host-deploy-probe".into(),
        arguments,
        manifest_id: record.manifest_id.clone(),
        artifact_digest: record.artifact_digest.clone(),
        registry_snapshot_id: record.target_registry_snapshot_id.clone(),
    };
    let stdin = serde_json::to_string(&protocol::build_process_request(&request))
        .map_err(|_| DeployError::ProbeFailed)?;
    let output = run_artifact(
        artifact,
        &stdin,
        config.exec_timeout,
        config.max_stdout_bytes,
        config.max_stderr_bytes,
    )
    .map_err(|_| DeployError::ProbeFailed)?;
    let (ok, response) = protocol::map_process_response(&output.stdout);
    let result = response.get("result").ok_or(DeployError::ProbeFailed)?;
    if output.exit_code != Some(0) || !ok {
        return Err(DeployError::ProbeFailed);
    }
    validate_output(record, result).map_err(|_| DeployError::ProbeFailed)
}

fn matches_request(record: &DeploymentRecord, request: &DeployRequest) -> bool {
    record.proposal_id == request.proposal_id
        && record.decision_id == request.decision_id
        && record.manifest_digest == request.manifest_digest
        && record.artifact_digest == request.artifact_digest
        && record.target_registry_snapshot_id == request.target_registry_snapshot_id
}

fn response(record: &DeploymentRecord, replayed: bool) -> Value {
    json!({
        "protocol_version":"capability-deploy-v1",
        "ok":true,
        "replayed":replayed,
        "deployment_id":record.deployment_id,
        "proposal_id":record.proposal_id,
        "decision_id":record.decision_id,
        "manifest_digest":record.manifest_digest,
        "manifest_id":record.manifest_id,
        "artifact_digest":record.artifact_digest,
        "operation_name":record.operation_name,
        "target_registry_snapshot_id":record.target_registry_snapshot_id,
        "probe_execution_id":record.probe_execution_id,
    })
}

fn safe_operation(value: &str) -> bool {
    safe_component_id(value) && value.starts_with("external.")
}

fn safe_component_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn safe_read_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 256
        && !value.contains(['?', '#', '\r', '\n'])
        && !value.split('/').any(|part| part == "..")
}

fn map_artifact_error(error: ArtifactError) -> DeployError {
    match error {
        ArtifactError::InvalidDigest => DeployError::Invalid("artifact_digest_invalid"),
        ArtifactError::NotFound | ArtifactError::DigestMismatch => {
            DeployError::Invalid("artifact_not_found_or_mismatched")
        }
        ArtifactError::UnsafeMaterializationRoot | ArtifactError::MaterializationChanged => {
            DeployError::State
        }
        ArtifactError::StoreError(_) => DeployError::State,
    }
}

#[derive(Debug)]
pub enum DeployError {
    Invalid(&'static str),
    NotDeployed,
    BindingMismatch,
    Conflict,
    DescriptorInvalid,
    ProbeFailed,
    UpstreamFailed,
    State,
}

impl DeployError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(code) => code,
            Self::NotDeployed => "capability_not_deployed",
            Self::BindingMismatch => "deployment_binding_mismatch",
            Self::Conflict => "deployment_conflict",
            Self::DescriptorInvalid => "capability_descriptor_invalid",
            Self::ProbeFailed => "deployment_probe_failed",
            Self::UpstreamFailed => "capability_upstream_unavailable",
            Self::State => "deployment_state_error",
        }
    }
}
