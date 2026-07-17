use super::{model, DevelopmentRequest, GenerationError, Value};
use super::{CARGO_LOCK, CARGO_TOML, ENTRY, MAIN_RS, SUPPORT_RS, TEST_KIT};
use crate::self_evolution::acceptance_selector::AcceptanceSelection;
use serde_json::json;
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
        "kind": "hook_consumer_service",
        "profile_id": request.build_profile,
        "contract_catalog_version": request.contract_catalog_version,
        "required_contracts": request.required_contracts,
        "requested_permissions": request.requested_permissions,
        "test_kit": TEST_KIT,
        "deployment_profile": request.deployment_profile,
        "entry": ENTRY,
        "artifact_digest": format!("sha256:{}", "0".repeat(64)),
        "acceptance_bundle_ref": selection.bundle_ref,
        "acceptance_bundle_digest": selection.bundle_digest,
        "service": {
            "version": "0.1.0",
            "healthcheck_path": "/health"
        },
        "generation": {
            "kind": "request-driven-model-module-v0",
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
) -> Result<(), GenerationError> {
    let model_name = manifest
        .pointer("/generation/model")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(|ch| ch.is_control())
        })
        .ok_or_else(cache_invalid)?;
    let source_digest = format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())));
    if manifest != &component_manifest(request, &source_digest, model_name, selection) {
        return Err(cache_invalid());
    }

    let stored_specification: Value =
        serde_json::from_slice(&std::fs::read(candidate.join("specification.json"))?)
            .map_err(|_| cache_invalid())?;
    if stored_specification != specification(request) {
        return Err(cache_invalid());
    }

    let runtime = MAIN_RS.replace("__COMPONENT_PRELUDE__", &model::component_prelude(source)?);
    for (path, expected) in [
        ("Cargo.toml", CARGO_TOML.as_bytes()),
        ("Cargo.lock", CARGO_LOCK.as_bytes()),
        ("src/main.rs", runtime.as_bytes()),
        ("src/support.rs", SUPPORT_RS.as_bytes()),
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
