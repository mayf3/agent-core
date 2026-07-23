use super::{render_runtime, CARGO_LOCK, CARGO_TOML, ENTRY, TEST_KIT};
use crate::self_evolution::acceptance_selector::AcceptanceSelection;
use crate::self_evolution::generator::GenerationError;
use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) fn component_manifest(
    request: &DevelopmentRequest,
    source_digest: &str,
    model_name: &str,
    selection: &AcceptanceSelection,
) -> Value {
    json!({
        "schema_version": "component-artifact-v1",
        "component_id": request.name,
        "kind": "invocable_capability",
        "profile_id": request.build_profile,
        "contract_catalog_version": request.contract_catalog_version,
        "required_contracts": request.required_contracts,
        "requested_permissions": request.requested_permissions,
        "test_kit": TEST_KIT,
        "deployment_profile": request.deployment_profile,
        "runtime_profile": "process-harness-v1",
        "healthcheck": "trusted process invocation",
        "rollback_policy": "reactivate previous content-addressed snapshot",
        "entry": ENTRY,
        "artifact_digest": format!("sha256:{}", "0".repeat(64)),
        "acceptance_bundle_ref": selection.bundle_ref,
        "acceptance_bundle_digest": selection.bundle_digest,
        "capability": {
            "operation_name": request.name,
            "description": format!("Governed generated capability {}.", request.name),
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            },
            "output_schema": {
                "type": "object",
                "properties": {
                    "status": {"type": "string", "enum": ["no_failures"]},
                    "capability_name": {"type": "string"},
                    "failed_stage": {"type": "string"},
                    "error_category": {"type": "string"},
                    "detail_code": {"type": "string"},
                    "run_id": {"type": "string"},
                    "invocation_id": {"type": "string"},
                    "receipt_status": {"type": "string"},
                    "receipt_time": {"type": "string"},
                    "source_component": {"type": "string"}
                },
                "required": ["source_component"],
                "additionalProperties": false
            },
            "idempotent": true
        },
        "generation": {
            "kind": "request-driven-model-transform-v0",
            "development_request_id": request.request_id,
            "model": model_name,
            "module_digest": source_digest,
            "mutable_surface": ["src/component.rs"]
        }
    })
}

pub(super) fn specification(request: &DevelopmentRequest) -> Value {
    json!({
        "schema_version": "generated-component-spec-v0",
        "development_request_id": request.request_id,
        "name": request.name,
        "target_kind": request.target_kind,
        "requirements": request.requirements,
        "required_contracts": request.required_contracts,
        "requested_permissions": request.requested_permissions,
        "component_profile": request.build_profile,
        "deployment_profile": request.deployment_profile,
        "acceptance_criteria": request.acceptance_criteria,
    })
}

pub(super) fn validate(
    candidate: &Path,
    request: &DevelopmentRequest,
    source: &str,
    manifest: &Value,
    selection: &AcceptanceSelection,
    upstream_component: &str,
    upstream_path: &str,
) -> Result<(), GenerationError> {
    let model_name = manifest
        .pointer("/generation/model")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        .ok_or_else(cache_invalid)?;
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())));
    if manifest != &component_manifest(request, &digest, model_name, selection) {
        return Err(cache_invalid());
    }
    let stored: Value =
        serde_json::from_slice(&std::fs::read(candidate.join("specification.json"))?)
            .map_err(|_| cache_invalid())?;
    if stored != specification(request) {
        return Err(cache_invalid());
    }
    for (path, expected) in [
        ("Cargo.toml", CARGO_TOML.as_bytes().to_vec()),
        ("Cargo.lock", CARGO_LOCK.as_bytes().to_vec()),
        (
            "src/main.rs",
            render_runtime(&request.name, upstream_component, upstream_path).into_bytes(),
        ),
    ] {
        if std::fs::read(candidate.join(path))? != expected {
            return Err(cache_invalid());
        }
    }
    Ok(())
}

fn cache_invalid() -> GenerationError {
    GenerationError::new("CANDIDATE_CACHE_INVALID")
}
