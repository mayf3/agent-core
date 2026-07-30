//! External profile acceptance for generated development candidates.
//!
//! The Coding Harness owns gate selection and evidence. The Kernel receives
//! only a generic, digest-bound acceptance receipt and opaque content refs.

use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::domain::{
    compute_acceptance_binding_digest, compute_external_receipt_digest, DevelopmentRequest,
    ExternalOutcome, ExternalReceiptEnvelope, TargetKind, SCHEMA_VERSION,
    TRUSTED_ACCEPTANCE_ISSUER,
};
use agent_core_kernel::harness::manifest::HarnessManifest;
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::hcr::acceptance::execution_store::ExecutionStore;
use crate::hcr::acceptance::protocol::RequestFingerprint;
use crate::hcr::candidate::snapshot_candidate;
use crate::hcr::gates::run_all_gates_for_acceptance;
use crate::hcr::manifest_builder::{allocate_next_version, build_delivery_manifest};

pub(super) const PROFILE_CATALOG_VERSION: &str = "component-profile-catalog-v1";

pub(super) fn accept(
    artifact_root: &Path,
    args: &Value,
    request: &DevelopmentRequest,
    generated: &Value,
) -> Result<Value, String> {
    let invocation_intent_id = required(args, "invocation_intent_id")?;
    let candidate_ref = required(generated, "candidate_ref")?;
    let request_digest = request_digest(request)?;
    let fingerprint = RequestFingerprint(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&json!({
                "request_digest": request_digest,
                "candidate_ref": candidate_ref,
                "invocation_intent_id": invocation_intent_id,
                "profile_id": request.build_profile,
                "profile_catalog_version": PROFILE_CATALOG_VERSION,
            }))
            .map_err(|_| "ACCEPTANCE_FINGERPRINT_FAILED")?
        ))
    ));
    let store = ExecutionStore::new(artifact_root);
    let result = store
        .execute(&request.idempotency_key, &fingerprint, || {
            execute(
                artifact_root,
                request,
                candidate_ref,
                invocation_intent_id,
                &request_digest,
            )
        })
        .map_err(|error| format!("PROFILE_ACCEPTANCE_STORE:{error}"))?;
    if let Some(error) = result.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(result)
}

fn execute(
    artifact_root: &Path,
    request: &DevelopmentRequest,
    candidate_ref: &str,
    invocation_intent_id: &str,
    request_digest: &str,
) -> Result<Value, String> {
    let candidate_path = safe_candidate_path(artifact_root, candidate_ref)?;
    let base_dir = artifact_root.join("candidates_base");
    std::fs::create_dir_all(&base_dir).map_err(|_| "CANDIDATE_BASE_CREATE_FAILED")?;
    let snapshot =
        snapshot_candidate(&candidate_path, &base_dir).map_err(|_| "CANDIDATE_SNAPSHOT_FAILED")?;
    let gate_run = run_all_gates_for_acceptance(&snapshot);
    let passed = gate_run.results.iter().all(|result| result.passed);
    if !passed {
        return Err("PROFILE_ACCEPTANCE_FAILED".into());
    }
    let artifact_bytes = gate_run
        .artifact_bytes
        .as_deref()
        .ok_or_else(|| "ACCEPTED_ARTIFACT_BYTES_MISSING".to_string())?;
    let store = ContentStore::new(artifact_root.to_path_buf());
    let artifact_digest = store
        .store(artifact_bytes)
        .map_err(|_| "ARTIFACT_STORE_FAILED")?
        .as_str()
        .to_string();
    let gate_artifact = gate_run
        .results
        .iter()
        .find_map(|result| result.computed_artifact_digest.as_deref())
        .ok_or_else(|| "ARTIFACT_GATE_DIGEST_MISSING".to_string())?;
    if gate_artifact != artifact_digest {
        return Err("ARTIFACT_STORE_DIGEST_MISMATCH".into());
    }

    let component_bytes = std::fs::read(snapshot.candidate_path.join("manifest.json"))
        .map_err(|_| "COMPONENT_MANIFEST_READ_FAILED")?;
    let mut component: Value =
        serde_json::from_slice(&component_bytes).map_err(|_| "COMPONENT_MANIFEST_INVALID")?;
    let (manifest_ref, manifest_bytes) = match request.target_kind {
        TargetKind::InvocableCapability => {
            let manifest = build_invocable_manifest(&component, &artifact_digest, request)?;
            (
                manifest.manifest_id.clone(),
                serde_json::to_vec(&manifest).map_err(|_| "MANIFEST_SERIALIZE_FAILED")?,
            )
        }
        TargetKind::HookConsumerService => {
            if let Some(version) = component
                .get("component_id")
                .and_then(Value::as_str)
                .and_then(|component_id| allocate_next_version(component_id).ok().flatten())
            {
                component["service"]["version"] = json!(version);
            }
            let manifest = build_delivery_manifest(&component, &artifact_digest)
                .map_err(|_| "DELIVERY_MANIFEST_BUILD_FAILED")?;
            (
                manifest.manifest_id.clone(),
                serde_json::to_vec(&manifest).map_err(|_| "MANIFEST_SERIALIZE_FAILED")?,
            )
        }
        _ => return Err("DEPLOYMENT_PROFILE_NOT_IMPLEMENTED".into()),
    };
    let manifest_digest = store
        .store(&manifest_bytes)
        .map_err(|_| "MANIFEST_STORE_FAILED")?
        .as_str()
        .to_string();

    let evidence = json!({
        "request_digest": request_digest,
        "candidate_id": snapshot.candidate_id,
        "candidate_digest": snapshot.candidate_digest,
        "artifact_digest": artifact_digest,
        "manifest_digest": manifest_digest,
        "profile_id": request.build_profile,
        "profile_catalog_version": PROFILE_CATALOG_VERSION,
        "gate_results": gate_run.results.iter().map(|result| result.to_json()).collect::<Vec<_>>(),
        "outcome": "passed",
    });
    let evidence_digest = store
        .store(&serde_json::to_vec(&evidence).map_err(|_| "EVIDENCE_SERIALIZE_FAILED")?)
        .map_err(|_| "EVIDENCE_STORE_FAILED")?
        .as_str()
        .to_string();
    let outcome = ExternalOutcome::Passed;
    let binding_digest = compute_acceptance_binding_digest(
        request_digest,
        &snapshot.candidate_digest,
        &artifact_digest,
        &manifest_digest,
        outcome,
        &request.contract_catalog_version,
        &request.build_profile,
        PROFILE_CATALOG_VERSION,
    );
    let receipt_digest = compute_external_receipt_digest(
        SCHEMA_VERSION,
        invocation_intent_id,
        TRUSTED_ACCEPTANCE_ISSUER,
        &artifact_digest,
        outcome,
        &evidence_digest,
        Some(&binding_digest),
    );
    let envelope = ExternalReceiptEnvelope {
        schema_version: SCHEMA_VERSION.into(),
        invocation_intent_id: invocation_intent_id.into(),
        issuer: TRUSTED_ACCEPTANCE_ISSUER.into(),
        subject_digest: artifact_digest.clone(),
        outcome,
        evidence_digest: evidence_digest.clone(),
        opaque_payload_digest: Some(binding_digest),
        receipt_digest: receipt_digest.clone(),
    };
    envelope
        .validate_structure()
        .and_then(|_| envelope.verify_receipt_digest())
        .map_err(|_| "ACCEPTANCE_RECEIPT_INVALID")?;

    Ok(json!({
        "request_id": request.request_id,
        "request_digest": request_digest,
        "candidate_id": snapshot.candidate_id,
        "candidate_ref": candidate_ref,
        "candidate_digest": snapshot.candidate_digest,
        "artifact_ref": artifact_digest,
        "artifact_digest": artifact_digest,
        "manifest_ref": manifest_ref,
        "manifest_digest": manifest_digest,
        "evidence_digest": evidence_digest,
        "acceptance_outcome": "passed",
        "contract_catalog_version": request.contract_catalog_version,
        "profile_id": request.build_profile,
        "profile_catalog_version": PROFILE_CATALOG_VERSION,
        "acceptance_receipt": envelope,
        "receipt_digest": receipt_digest,
    }))
}

fn request_digest(request: &DevelopmentRequest) -> Result<String, String> {
    serde_json::to_vec(request)
        .map(|bytes| Sha256Digest::compute(&bytes).as_str().to_string())
        .map_err(|_| "DEVELOPMENT_REQUEST_DIGEST_FAILED".into())
}

fn safe_candidate_path(root: &Path, candidate_ref: &str) -> Result<std::path::PathBuf, String> {
    if candidate_ref.contains("..") || Path::new(candidate_ref).is_absolute() {
        return Err("CANDIDATE_REF_ESCAPE".into());
    }
    let path = root.join(candidate_ref);
    if !path.is_dir() {
        return Err("CANDIDATE_NOT_FOUND".into());
    }
    Ok(path)
}

fn build_invocable_manifest(
    component: &Value,
    artifact_digest: &str,
    request: &DevelopmentRequest,
) -> Result<HarnessManifest, String> {
    if request.target_kind != TargetKind::InvocableCapability
        || required(component, "schema_version")? != "component-artifact-v1"
        || required(component, "kind")? != "invocable_capability"
        || required(component, "component_id")? != request.name
        || required(component, "profile_id")? != request.build_profile
        || required(component, "contract_catalog_version")? != request.contract_catalog_version
        || required(component, "deployment_profile")? != request.deployment_profile
    {
        return Err("COMPONENT_MANIFEST_IDENTITY_MISMATCH".into());
    }
    let capability = component
        .get("capability")
        .filter(|value| value.is_object())
        .ok_or_else(|| "CAPABILITY_MANIFEST_MISSING".to_string())?;
    if required(capability, "operation_name")? != request.name {
        return Err("CAPABILITY_OPERATION_MISMATCH".into());
    }
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "capability-host-v0".into(),
        artifact_digest: artifact_digest.into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:7300/execute".into(),
        operation_name: request.name.clone(),
        description: required(capability, "description")?.into(),
        input_schema: capability
            .get("input_schema")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| "CAPABILITY_INPUT_SCHEMA_MISSING".to_string())?,
        output_schema: capability
            .get("output_schema")
            .cloned()
            .ok_or_else(|| "CAPABILITY_OUTPUT_SCHEMA_MISSING".to_string())?,
        idempotent: capability
            .get("idempotent")
            .and_then(Value::as_bool)
            .ok_or_else(|| "CAPABILITY_IDEMPOTENCY_MISSING".to_string())?,
        created_at: Utc::now(),
    };
    manifest.manifest_id = manifest
        .compute_manifest_id()
        .map_err(|_| "MANIFEST_ID_FAILED")?;
    manifest
        .validate_all()
        .map_err(|_| "HARNESS_MANIFEST_INVALID")?;
    Ok(manifest)
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("MISSING_{key}"))
}
