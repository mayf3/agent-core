//! Durable, calculator-only deployment allowlist.

use crate::artifact::{resolve_artifact, ArtifactError, ResolvedArtifact};
use crate::config::CapabilityHostConfig;
use crate::process::run_artifact;
use crate::protocol::{self, HarnessRequest};
use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::harness::manifest::HarnessManifest;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

const OPERATION: &str = "external.calculator";

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
}

pub fn prepare(config: &CapabilityHostConfig, body: &str) -> Result<Value, DeployError> {
    let request: DeployRequest =
        serde_json::from_str(body).map_err(|_| DeployError::Invalid("malformed_deploy_request"))?;
    validate_identity(&request)?;
    let _deployment_lock = lock_deploy(config)?;
    if let Some(existing) = load(config)? {
        if matches_request(&existing, &request) {
            return Ok(response(&existing, true));
        }
        return Err(DeployError::Conflict);
    }
    let manifest = load_manifest(&config.artifact_root, &request.manifest_digest)?;
    validate_manifest(config, &request, &manifest)?;
    let artifact = resolve_artifact(&config.artifact_root, &request.artifact_digest)
        .map_err(map_artifact_error)?;

    let mut record = DeploymentRecord {
        deployment_id: deployment_id(&request, &manifest.manifest_id),
        proposal_id: request.proposal_id,
        decision_id: request.decision_id,
        manifest_digest: request.manifest_digest,
        manifest_id: manifest.manifest_id,
        artifact_digest: request.artifact_digest,
        operation_name: OPERATION.into(),
        target_registry_snapshot_id: request.target_registry_snapshot_id,
        probe_execution_id: String::new(),
    };
    record.probe_execution_id = execution_id(
        &record.deployment_id,
        "capability-host-deploy-probe",
        &json!({"operation":"multiply","a":6,"b":7}),
    );
    probe(config, &artifact, &record)?;
    persist(config, &record)?;
    Ok(response(&record, false))
}

fn matches_request(record: &DeploymentRecord, request: &DeployRequest) -> bool {
    record.proposal_id == request.proposal_id
        && record.decision_id == request.decision_id
        && record.manifest_digest == request.manifest_digest
        && record.artifact_digest == request.artifact_digest
        && record.target_registry_snapshot_id == request.target_registry_snapshot_id
}

pub fn authorize_execution(
    config: &CapabilityHostConfig,
    request: &HarnessRequest,
) -> Result<DeploymentRecord, DeployError> {
    validate_calculator_arguments(&request.arguments)?;
    let record = load(config)?.ok_or(DeployError::NotDeployed)?;
    if request.operation_name != record.operation_name
        || request.manifest_id != record.manifest_id
        || request.artifact_digest != record.artifact_digest
        || request.registry_snapshot_id != record.target_registry_snapshot_id
    {
        return Err(DeployError::BindingMismatch);
    }
    Ok(record)
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
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| DeployError::Invalid("manifest_invalid"))?;
    let object = value
        .as_object()
        .ok_or(DeployError::Invalid("manifest_invalid"))?;
    const FIELDS: [&str; 10] = [
        "manifest_id",
        "harness_id",
        "artifact_digest",
        "protocol_version",
        "endpoint",
        "operation_name",
        "description",
        "input_schema",
        "output_schema",
        "idempotent",
    ];
    if object.len() != FIELDS.len() + 1
        || !FIELDS.iter().all(|field| object.contains_key(*field))
        || !object.contains_key("created_at")
    {
        return Err(DeployError::Invalid("manifest_invalid"));
    }
    serde_json::from_value(value).map_err(|_| DeployError::Invalid("manifest_invalid"))
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
        || manifest.operation_name != OPERATION
        || manifest.protocol_version != "external-harness-v1"
        || manifest.artifact_digest != request.artifact_digest
        || manifest.description
            != "Approved calculator supporting add, subtract, multiply, and divide."
        || !manifest.idempotent
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
    if manifest.input_schema != calculator_input_schema()
        || manifest.output_schema != json!({"type":"number"})
    {
        return Err(DeployError::Invalid("calculator_schema_invalid"));
    }
    Ok(())
}

fn calculator_input_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "operation":{"type":"string","enum":["add","subtract","multiply","divide"]},
            "a":{"type":"number"},
            "b":{"type":"number"}
        },
        "required":["operation","a","b"],
        "additionalProperties":false
    })
}

fn validate_calculator_arguments(arguments: &Value) -> Result<(), DeployError> {
    let object = arguments
        .as_object()
        .ok_or(DeployError::Invalid("invalid_calculator_arguments"))?;
    if object.len() != 3
        || !matches!(
            object.get("operation").and_then(Value::as_str),
            Some("add" | "subtract" | "multiply" | "divide")
        )
        || !object.get("a").is_some_and(Value::is_number)
        || !object.get("b").is_some_and(Value::is_number)
    {
        return Err(DeployError::Invalid("invalid_calculator_arguments"));
    }
    Ok(())
}

fn probe(
    config: &CapabilityHostConfig,
    artifact: &ResolvedArtifact,
    record: &DeploymentRecord,
) -> Result<(), DeployError> {
    let request = HarnessRequest {
        protocol_version: "external-harness-v1".into(),
        operation_name: OPERATION.into(),
        invocation_id: "capability-host-deploy-probe".into(),
        arguments: json!({"operation":"multiply","a":6,"b":7}),
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
    if output.exit_code != Some(0) || !ok || response.get("result") != Some(&json!(42)) {
        return Err(DeployError::ProbeFailed);
    }
    Ok(())
}

fn state_path(config: &CapabilityHostConfig) -> PathBuf {
    config
        .artifact_root
        .join(".capability-host")
        .join("external.calculator.json")
}

fn lock_deploy(config: &CapabilityHostConfig) -> Result<std::fs::File, DeployError> {
    let parent = state_path(config)
        .parent()
        .ok_or(DeployError::State)?
        .to_path_buf();
    std::fs::create_dir_all(&parent).map_err(|_| DeployError::State)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(parent.join("deployment.lock"))
        .map_err(|_| DeployError::State)?;
    file.lock_exclusive().map_err(|_| DeployError::State)?;
    Ok(file)
}

fn load(config: &CapabilityHostConfig) -> Result<Option<DeploymentRecord>, DeployError> {
    let path = state_path(config);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(DeployError::State),
    };
    let record: DeploymentRecord =
        serde_json::from_slice(&bytes).map_err(|_| DeployError::State)?;
    let expected_probe = execution_id(
        &record.deployment_id,
        "capability-host-deploy-probe",
        &json!({"operation":"multiply","a":6,"b":7}),
    );
    if record.deployment_id != deployment_id_from_record(&record)
        || record.probe_execution_id != expected_probe
        || record.operation_name != OPERATION
    {
        return Err(DeployError::State);
    }
    Ok(Some(record))
}

fn persist(config: &CapabilityHostConfig, record: &DeploymentRecord) -> Result<(), DeployError> {
    let path = state_path(config);
    let parent = path.parent().ok_or(DeployError::State)?;
    std::fs::create_dir_all(parent).map_err(|_| DeployError::State)?;
    let temp = parent.join(format!(".deploy.{}.tmp", std::process::id()));
    let mut file = std::fs::File::create(&temp).map_err(|_| DeployError::State)?;
    let bytes = serde_json::to_vec(record).map_err(|_| DeployError::State)?;
    file.write_all(&bytes).map_err(|_| DeployError::State)?;
    file.sync_all().map_err(|_| DeployError::State)?;
    std::fs::rename(&temp, &path).map_err(|_| DeployError::State)?;
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| DeployError::State)?;
    Ok(())
}

fn deployment_id(request: &DeployRequest, manifest_id: &str) -> String {
    deployment_hash(json!({
        "proposal_id":request.proposal_id,
        "decision_id":request.decision_id,
        "manifest_digest":request.manifest_digest,
        "manifest_id":manifest_id,
        "artifact_digest":request.artifact_digest,
        "operation_name":OPERATION,
        "target_registry_snapshot_id":request.target_registry_snapshot_id,
    }))
}

fn deployment_id_from_record(record: &DeploymentRecord) -> String {
    deployment_hash(json!({
        "proposal_id":record.proposal_id,
        "decision_id":record.decision_id,
        "manifest_digest":record.manifest_digest,
        "manifest_id":record.manifest_id,
        "artifact_digest":record.artifact_digest,
        "operation_name":record.operation_name,
        "target_registry_snapshot_id":record.target_registry_snapshot_id,
    }))
}

fn deployment_hash(value: Value) -> String {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    format!("chd_{}", hex::encode(Sha256::digest(bytes)))
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
        "target_registry_snapshot_id":record.target_registry_snapshot_id,
        "probe_execution_id":record.probe_execution_id,
    })
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
    ProbeFailed,
    State,
}

impl DeployError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(code) => code,
            Self::NotDeployed => "capability_not_deployed",
            Self::BindingMismatch => "deployment_binding_mismatch",
            Self::Conflict => "deployment_conflict",
            Self::ProbeFailed => "deployment_probe_failed",
            Self::State => "deployment_state_error",
        }
    }
}
