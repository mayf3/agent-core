use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::domain::{
    ComponentControlReceipt, DeploymentIntent, DeploymentReceipt, ServiceManifest,
    DEPLOYMENT_PROTOCOL,
};
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::DeploymentHarnessConfig;
use crate::process;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentRecord {
    intent: DeploymentIntent,
    manifest: ServiceManifest,
    receipt: DeploymentReceipt,
    executable_path: String,
    pid: u32,
    port: u16,
    instance_id: String,
    status: String,
    #[serde(default)]
    last_control_receipt: Option<ComponentControlReceipt>,
}

pub fn deploy(config: &DeploymentHarnessConfig, body: &[u8]) -> Result<DeploymentReceipt> {
    let intent: DeploymentIntent =
        serde_json::from_slice(body).map_err(|_| anyhow!("DEPLOYMENT_INTENT_MALFORMED"))?;
    intent.validate()?;
    let manifest = load_manifest(config, &intent)?;
    let _lock = lock_component(config, &manifest.component_id)?;
    let active = load_active(config, &manifest.component_id)?;
    if let Some(existing) = &active {
        validate_record(config, existing)?;
        if existing.intent == intent {
            let mut receipt = existing.receipt.clone();
            if existing.status == "healthy" && record_is_healthy(existing) {
                receipt.replayed = true;
                return Ok(receipt);
            }
        } else if compare_version(&manifest.version, &existing.manifest.version)
            != std::cmp::Ordering::Greater
        {
            bail!("DEPLOYMENT_VERSION_NOT_MONOTONIC");
        }
    }

    let artifact = ContentStore::new(config.artifact_root.clone()).load(
        &Sha256Digest::parse(&intent.artifact_digest)
            .map_err(|_| anyhow!("DEPLOYMENT_ARTIFACT_DIGEST_INVALID"))?,
    )?;
    let deployment_id = intent.deployment_id(&manifest.component_id);
    let component_root = component_root(config, &manifest.component_id);
    let version_root = component_root
        .join("versions")
        .join(&manifest.version)
        .join(digest_suffix(&manifest.artifact_digest));
    let executable = version_root.join(&manifest.entrypoint);
    if !executable.exists() {
        process::install_artifact(&artifact, &executable)?;
    }
    let started_at = Utc::now().to_rfc3339();
    let started = process::start(
        config,
        &manifest.component_id,
        &manifest.version,
        &executable,
        &component_root.join(&manifest.state_path),
        &manifest.healthcheck.path,
        Duration::from_millis(manifest.healthcheck.timeout_ms),
        None,
    )?;
    let log_ref = relative_log_ref(config, &started.log_path)?;
    let previous_artifact_digest = active
        .as_ref()
        .map(|record| record.manifest.artifact_digest.clone());
    let mut receipt = DeploymentReceipt {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        receipt_id: String::new(),
        invocation_id: intent.invocation_id.clone(),
        intent_id: intent.intent_id.clone(),
        proposal_id: intent.proposal_id.clone(),
        decision_id: intent.decision_id.clone(),
        deployment_id,
        component_id: manifest.component_id.clone(),
        service_manifest_digest: intent.service_manifest_digest.clone(),
        artifact_digest: intent.artifact_digest.clone(),
        version: manifest.version.clone(),
        status: "healthy".into(),
        endpoint: started.endpoint,
        health_status: "ready".into(),
        log_ref,
        previous_artifact_digest,
        started_at,
        finished_at: Utc::now().to_rfc3339(),
        replayed: false,
    };
    receipt.receipt_id = receipt.expected_receipt_id();
    if let Err(error) = receipt.validate_for(&intent, &manifest.component_id) {
        process::stop(started.pid, &executable);
        return Err(error);
    }
    let record = DeploymentRecord {
        intent,
        manifest,
        receipt: receipt.clone(),
        executable_path: executable.to_string_lossy().into_owned(),
        pid: started.pid,
        port: started.port,
        instance_id: started.instance_id,
        status: "healthy".into(),
        last_control_receipt: None,
    };
    if let Err(error) = persist_record(config, &record) {
        process::stop(started.pid, &executable);
        return Err(error);
    }
    if let Some(previous) = active {
        if !stop_record(&previous) {
            persist_active(config, &previous)?;
            process::stop(started.pid, &executable);
            bail!("SERVICE_PREVIOUS_STOP_FAILED");
        }
    }
    Ok(receipt)
}

pub fn status(config: &DeploymentHarnessConfig, component_id: &str) -> Result<Value> {
    validate_component_id(component_id)?;
    let record =
        load_active(config, component_id)?.ok_or_else(|| anyhow!("COMPONENT_NOT_DEPLOYED"))?;
    validate_record(config, &record)?;
    let ready = record.status != "disabled" && record_is_healthy(&record);
    Ok(json!({
        "protocol_version": DEPLOYMENT_PROTOCOL,
        "ok": true,
        "component_id": component_id,
        "deployment_id": record.receipt.deployment_id,
        "artifact_digest": record.receipt.artifact_digest,
        "version": record.receipt.version,
        "endpoint": record.receipt.endpoint,
        "status": if record.status == "healthy" && ready { "healthy" } else { record.status.as_str() },
        "health_status": if ready { "ready" } else { "unavailable" },
        "log_ref": record.receipt.log_ref,
    }))
}

pub fn disable(
    config: &DeploymentHarnessConfig,
    component_id: &str,
    decision_id: &str,
) -> Result<ComponentControlReceipt> {
    validate_control(component_id, decision_id)?;
    let _lock = lock_component(config, component_id)?;
    let mut record =
        load_active(config, component_id)?.ok_or_else(|| anyhow!("COMPONENT_NOT_DEPLOYED"))?;
    validate_record(config, &record)?;
    if let Some(receipt) = replay_control(&record, "disable", decision_id, component_id)? {
        return Ok(receipt);
    }
    if record.status != "disabled" {
        if !stop_record(&record) {
            bail!("SERVICE_STOP_FAILED");
        }
        record.status = "disabled".into();
    }
    let receipt = control_receipt("disable", decision_id, &record, "disabled", "unavailable");
    record.last_control_receipt = Some(receipt.clone());
    persist_active(config, &record)?;
    Ok(receipt)
}

pub fn rollback(
    config: &DeploymentHarnessConfig,
    component_id: &str,
    decision_id: &str,
) -> Result<ComponentControlReceipt> {
    validate_control(component_id, decision_id)?;
    let _lock = lock_component(config, component_id)?;
    let current =
        load_active(config, component_id)?.ok_or_else(|| anyhow!("COMPONENT_NOT_DEPLOYED"))?;
    validate_record(config, &current)?;
    if let Some(receipt) = replay_control(&current, "rollback", decision_id, component_id)? {
        return Ok(receipt);
    }
    let previous_digest = current
        .receipt
        .previous_artifact_digest
        .as_deref()
        .ok_or_else(|| anyhow!("ROLLBACK_TARGET_MISSING"))?;
    let mut previous = load_record_by_artifact(config, component_id, previous_digest)?
        .ok_or_else(|| anyhow!("ROLLBACK_TARGET_NOT_FOUND"))?;
    validate_record(config, &previous)?;
    let executable = PathBuf::from(&previous.executable_path);
    let started = process::start(
        config,
        component_id,
        &previous.manifest.version,
        &executable,
        &component_root(config, component_id).join(&previous.manifest.state_path),
        &previous.manifest.healthcheck.path,
        Duration::from_millis(previous.manifest.healthcheck.timeout_ms),
        None,
    )?;
    previous.pid = started.pid;
    previous.port = started.port;
    previous.instance_id = started.instance_id;
    previous.status = "rolled_back".into();
    previous.receipt.endpoint = started.endpoint;
    previous.receipt.health_status = "ready".into();
    previous.receipt.status = "healthy".into();
    previous.receipt.receipt_id = previous.receipt.expected_receipt_id();
    let receipt = control_receipt("rollback", decision_id, &previous, "rolled_back", "ready");
    previous.last_control_receipt = Some(receipt.clone());
    if let Err(error) = persist_active(config, &previous) {
        process::stop(started.pid, &executable);
        return Err(error);
    }
    if !stop_record(&current) {
        persist_active(config, &current)?;
        process::stop(started.pid, &executable);
        bail!("SERVICE_PREVIOUS_STOP_FAILED");
    }
    Ok(receipt)
}

fn load_manifest(
    config: &DeploymentHarnessConfig,
    intent: &DeploymentIntent,
) -> Result<ServiceManifest> {
    let store = ContentStore::new(config.artifact_root.clone());
    let bytes = store.load(
        &Sha256Digest::parse(&intent.service_manifest_digest)
            .map_err(|_| anyhow!("SERVICE_MANIFEST_DIGEST_INVALID"))?,
    )?;
    let manifest: ServiceManifest =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("SERVICE_MANIFEST_INVALID"))?;
    manifest.validate()?;
    if manifest.artifact_digest != intent.artifact_digest
        || manifest.version != intent.expected_version
    {
        bail!("SERVICE_MANIFEST_INTENT_MISMATCH");
    }
    Ok(manifest)
}

fn record_is_healthy(record: &DeploymentRecord) -> bool {
    process::probe(
        &format!("127.0.0.1:{}", record.port),
        &record.manifest.healthcheck.path,
        Duration::from_millis(500),
        &record.manifest.component_id,
        &record.manifest.version,
        &record.instance_id,
    )
    .is_success()
}

fn stop_record(record: &DeploymentRecord) -> bool {
    let stopped = process::stop(record.pid, Path::new(&record.executable_path));
    stopped || !record_is_healthy(record)
}

/// Restore every approved active service after a Harness or host restart.
/// Recovery is fail-closed and reuses the published port so the immutable
/// Kernel component snapshot remains a valid routing fact.
pub fn reconcile(config: &DeploymentHarnessConfig) -> Result<()> {
    let components_root = config.state_root.join("components");
    let entries = match std::fs::read_dir(&components_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let component_id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("COMPONENT_ID_INVALID"))?;
        validate_component_id(&component_id)?;
        let _lock = lock_component(config, &component_id)?;
        let Some(mut record) = load_active(config, &component_id)? else {
            continue;
        };
        validate_record(config, &record)?;
        if record.status == "disabled" || record_is_healthy(&record) {
            continue;
        }
        let executable = PathBuf::from(&record.executable_path);
        let started = process::start(
            config,
            &record.manifest.component_id,
            &record.manifest.version,
            &executable,
            &component_root(config, &component_id).join(&record.manifest.state_path),
            &record.manifest.healthcheck.path,
            Duration::from_millis(record.manifest.healthcheck.timeout_ms),
            Some(record.port),
        )?;
        if started.endpoint != record.receipt.endpoint {
            process::stop(started.pid, &executable);
            bail!("SERVICE_RECOVERY_ENDPOINT_CHANGED");
        }
        record.pid = started.pid;
        record.instance_id = started.instance_id;
        if let Err(error) = persist_active(config, &record) {
            process::stop(record.pid, &executable);
            return Err(error);
        }
    }
    Ok(())
}

fn validate_record(config: &DeploymentHarnessConfig, record: &DeploymentRecord) -> Result<()> {
    record.intent.validate()?;
    record.manifest.validate()?;
    record
        .receipt
        .validate_for(&record.intent, &record.manifest.component_id)?;
    if !matches!(
        record.status.as_str(),
        "healthy" | "rolled_back" | "disabled"
    ) || record.instance_id.len() != 73
        || !record.instance_id.starts_with("instance_")
        || !record.instance_id[9..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || record.receipt.endpoint != format!("http://127.0.0.1:{}", record.port)
    {
        bail!("DEPLOYMENT_RECORD_INVALID");
    }
    let expected = component_root(config, &record.manifest.component_id)
        .join("versions")
        .join(&record.manifest.version)
        .join(digest_suffix(&record.manifest.artifact_digest))
        .join(&record.manifest.entrypoint);
    if Path::new(&record.executable_path) != expected {
        bail!("DEPLOYMENT_RECORD_EXECUTABLE_INVALID");
    }
    Ok(())
}

fn persist_record(config: &DeploymentHarnessConfig, record: &DeploymentRecord) -> Result<()> {
    let root = component_root(config, &record.manifest.component_id);
    std::fs::create_dir_all(root.join("deployments"))?;
    persist_json(
        &root
            .join("deployments")
            .join(format!("{}.json", record.receipt.deployment_id)),
        record,
    )?;
    persist_active(config, record)
}

fn persist_active(config: &DeploymentHarnessConfig, record: &DeploymentRecord) -> Result<()> {
    persist_json(
        &component_root(config, &record.manifest.component_id).join("active.json"),
        record,
    )
}

fn persist_json(path: &Path, record: &DeploymentRecord) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("STATE_PATH_INVALID"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".record-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(record)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn load_active(
    config: &DeploymentHarnessConfig,
    component_id: &str,
) -> Result<Option<DeploymentRecord>> {
    load_record(&component_root(config, component_id).join("active.json"))
}

fn load_record(path: &Path) -> Result<Option<DeploymentRecord>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn load_record_by_artifact(
    config: &DeploymentHarnessConfig,
    component_id: &str,
    digest: &str,
) -> Result<Option<DeploymentRecord>> {
    let directory = component_root(config, component_id).join("deployments");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(None);
    };
    for entry in entries {
        let entry = entry?;
        if let Some(record) = load_record(&entry.path())? {
            if record.manifest.artifact_digest == digest {
                return Ok(Some(record));
            }
        }
    }
    Ok(None)
}

fn lock_component(config: &DeploymentHarnessConfig, component_id: &str) -> Result<File> {
    validate_component_id(component_id)?;
    let root = component_root(config, component_id);
    std::fs::create_dir_all(&root)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(root.join("deployment.lock"))?;
    file.lock_exclusive()?;
    Ok(file)
}

fn component_root(config: &DeploymentHarnessConfig, component_id: &str) -> PathBuf {
    config.state_root.join("components").join(component_id)
}

fn digest_suffix(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
}

fn compare_version(left: &str, right: &str) -> std::cmp::Ordering {
    let parse = |value: &str| {
        value
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(left).cmp(&parse(right))
}

fn relative_log_ref(config: &DeploymentHarnessConfig, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(&config.state_root)?
        .to_string_lossy()
        .into_owned())
}

fn validate_component_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        bail!("COMPONENT_ID_INVALID");
    }
    Ok(())
}

fn validate_control(component_id: &str, decision_id: &str) -> Result<()> {
    validate_component_id(component_id)?;
    if decision_id.is_empty()
        || decision_id.len() > 256
        || decision_id.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        bail!("CONTROL_DECISION_INVALID");
    }
    Ok(())
}

fn control_receipt(
    action: &str,
    decision_id: &str,
    record: &DeploymentRecord,
    status: &str,
    health_status: &str,
) -> ComponentControlReceipt {
    let mut receipt = ComponentControlReceipt {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        ok: true,
        receipt_id: String::new(),
        action: action.into(),
        decision_id: decision_id.into(),
        component_id: record.manifest.component_id.clone(),
        deployment_id: record.receipt.deployment_id.clone(),
        artifact_digest: record.manifest.artifact_digest.clone(),
        version: record.manifest.version.clone(),
        status: status.into(),
        endpoint: record.receipt.endpoint.clone(),
        health_status: health_status.into(),
        log_ref: record.receipt.log_ref.clone(),
    };
    receipt.receipt_id = receipt.expected_receipt_id();
    receipt
}

fn replay_control(
    record: &DeploymentRecord,
    action: &str,
    decision_id: &str,
    component_id: &str,
) -> Result<Option<ComponentControlReceipt>> {
    let Some(receipt) = &record.last_control_receipt else {
        return Ok(None);
    };
    if receipt.decision_id != decision_id {
        return Ok(None);
    }
    if receipt.action != action || receipt.component_id != component_id {
        bail!("CONTROL_DECISION_CONFLICT");
    }
    Ok(Some(receipt.clone()))
}
