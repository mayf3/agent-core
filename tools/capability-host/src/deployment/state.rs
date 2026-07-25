use super::{safe_operation, DeployError, DeployRequest, DeploymentRecord};
use crate::config::CapabilityHostConfig;
use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;

pub(super) fn lock(config: &CapabilityHostConfig) -> Result<std::fs::File, DeployError> {
    let parent = config.artifact_root.join(".capability-host");
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

pub(super) fn load(
    config: &CapabilityHostConfig,
    operation: &str,
) -> Result<Option<DeploymentRecord>, DeployError> {
    let bytes = match std::fs::read(state_path(config, operation)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(legacy) = legacy_state_path(config, operation) else {
                return Ok(None);
            };
            match std::fs::read(legacy) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(DeployError::State),
            }
        }
        Err(_) => return Err(DeployError::State),
    };
    let record: DeploymentRecord =
        serde_json::from_slice(&bytes).map_err(|_| DeployError::State)?;
    if record.deployment_id != deployment_id_from_record(&record)
        || record.operation_name != operation
    {
        return Err(DeployError::State);
    }
    if let Some(execution) = &record.execution {
        let expected = super::execution_id(
            &record.deployment_id,
            "capability-host-deploy-probe",
            &execution.probe_arguments,
        );
        if record.probe_execution_id != expected {
            return Err(DeployError::State);
        }
    }
    Ok(Some(record))
}

pub(super) fn persist(
    config: &CapabilityHostConfig,
    record: &DeploymentRecord,
) -> Result<(), DeployError> {
    let path = state_path(config, &record.operation_name);
    let parent = path.parent().ok_or(DeployError::State)?;
    std::fs::create_dir_all(parent).map_err(|_| DeployError::State)?;
    let temp = parent.join(format!(".deploy.{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|_| DeployError::State)?;
    file.write_all(&serde_json::to_vec(record).map_err(|_| DeployError::State)?)
        .and_then(|_| file.sync_all())
        .map_err(|_| DeployError::State)?;
    std::fs::rename(&temp, &path).map_err(|_| DeployError::State)?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| DeployError::State)
}

pub(super) fn deployment_id(request: &DeployRequest, manifest_id: &str, operation: &str) -> String {
    deployment_hash(json!({
        "proposal_id":request.proposal_id,
        "decision_id":request.decision_id,
        "manifest_digest":request.manifest_digest,
        "manifest_id":manifest_id,
        "artifact_digest":request.artifact_digest,
        "operation_name":operation,
        "target_registry_snapshot_id":request.target_registry_snapshot_id,
    }))
}

fn state_path(config: &CapabilityHostConfig, operation: &str) -> PathBuf {
    let digest = hex::encode(Sha256::digest(operation.as_bytes()));
    config
        .artifact_root
        .join(".capability-host")
        .join(format!("{}.json", &digest[..32]))
}

fn legacy_state_path(config: &CapabilityHostConfig, operation: &str) -> Option<PathBuf> {
    safe_operation(operation).then(|| {
        config
            .artifact_root
            .join(".capability-host")
            .join(format!("{operation}.json"))
    })
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
    format!(
        "chd_{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&value).unwrap_or_default()
        ))
    )
}
