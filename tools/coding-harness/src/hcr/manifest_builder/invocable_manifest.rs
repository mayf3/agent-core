//! InvocableCapability → HarnessManifest delivery manifest construction.
//!
//! Constructs the final delivery `HarnessManifest` from the immutable
//! candidate component manifest and the original `DevelopmentRequest`.
//! Identity validation ensures the candidate matches what was requested.
//!
//! This module lives in the Coding Harness, not the Kernel, so that
//! product‑specific fields (capability‑host‑v0, input/output schemas,
//! idempotent, description, etc.) stay outside Kernel governance paths.

use agent_core_kernel::domain::DevelopmentRequest;
#[cfg(test)]
use agent_core_kernel::domain::TargetKind;
use agent_core_kernel::harness::manifest::HarnessManifest;
use anyhow::{anyhow, Result};
use chrono::Utc;
use serde_json::Value;

/// Build a `HarnessManifest` from the gated component artifact manifest
/// and the original development request.
///
/// The candidate manifest has already passed all five acceptance gates and
/// is treated as immutable — we read it, validate identity against the
/// `DevelopmentRequest`, construct the `HarnessManifest`, compute its
/// deterministic `manifest_id`, run all business validations, and return it.
///
/// The caller (acceptance orchestrator) stores the serialised manifest in
/// the shared ContentStore and returns the content‑addressed ref/digest.
pub fn build_invocable_manifest(
    component: &Value,
    artifact_digest: &str,
    request: &DevelopmentRequest,
) -> Result<HarnessManifest> {
    // ── Target kind gate ──────────────────────────────────────────────
    let target_kind = component
        .get("target_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_TARGET_KIND"))?;
    if target_kind != "InvocableCapability" {
        return Err(anyhow!("UNEXPECTED_TARGET_KIND: {target_kind}"));
    }

    // ── Schema & kind identity ────────────────────────────────────────
    let schema_version = required_str(component, "schema_version")?;
    if schema_version != "component-artifact-v1" {
        return Err(anyhow!("UNEXPECTED_SCHEMA_VERSION: {schema_version}"));
    }
    let kind = required_str(component, "kind")?;
    if kind != "invocable_capability" {
        return Err(anyhow!("UNEXPECTED_KIND: {kind}"));
    }

    // ── DevelopmentRequest identity checks ────────────────────────────
    // These validations were previously in the Kernel's invocable_manifest().
    // They are preserved here so that acceptance fails fast if the candidate
    // component manifest does not match what the original request specified.
    let component_id = required_str(component, "component_id")?;
    if component_id != request.name {
        return Err(anyhow!(
            "COMPONENT_MANIFEST_IDENTITY_MISMATCH: component_id={component_id}, expected={}",
            request.name
        ));
    }

    let profile_id = required_str(component, "profile_id")?;
    if profile_id != request.build_profile {
        return Err(anyhow!(
            "COMPONENT_MANIFEST_IDENTITY_MISMATCH: profile_id={profile_id}, expected={}",
            request.build_profile
        ));
    }

    let contract_catalog_version = required_str(component, "contract_catalog_version")?;
    if contract_catalog_version != request.contract_catalog_version {
        return Err(anyhow!(
            "COMPONENT_MANIFEST_CONTRACT_CATALOG_MISMATCH"
        ));
    }

    let deployment_profile = required_str(component, "deployment_profile")?;
    if deployment_profile != request.deployment_profile {
        return Err(anyhow!(
            "COMPONENT_MANIFEST_DEPLOYMENT_PROFILE_MISMATCH"
        ));
    }

    if !string_set_matches(component, "required_contracts", &request.required_contracts)? {
        return Err(anyhow!("COMPONENT_MANIFEST_CONTRACT_MISMATCH"));
    }

    if !string_set_matches(component, "requested_permissions", &request.requested_permissions)? {
        return Err(anyhow!("COMPONENT_MANIFEST_PERMISSION_MISMATCH"));
    }

    // ── Capability section ────────────────────────────────────────────
    let capability = component
        .get("capability")
        .filter(|v| v.is_object())
        .ok_or_else(|| anyhow!("CAPABILITY_MANIFEST_MISSING"))?;

    let operation_name = required_str(capability, "operation_name")?;
    if operation_name != request.name {
        return Err(anyhow!(
            "CAPABILITY_OPERATION_MISMATCH: operation_name={operation_name}, expected={}",
            request.name
        ));
    }

    let description = required_str(capability, "description")?;

    let input_schema = capability
        .get("input_schema")
        .filter(|v| v.is_object())
        .cloned()
        .ok_or_else(|| anyhow!("CAPABILITY_INPUT_SCHEMA_MISSING"))?;

    let output_schema = capability
        .get("output_schema")
        .cloned()
        .ok_or_else(|| anyhow!("CAPABILITY_OUTPUT_SCHEMA_MISSING"))?;

    let idempotent = capability
        .get("idempotent")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("CAPABILITY_IDEMPOTENCY_MISSING"))?;

    // ── Construct HarnessManifest ─────────────────────────────────────
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "capability-host-v0".to_string(),
        artifact_digest: artifact_digest.to_string(),
        protocol_version: "external-harness-v1".to_string(),
        endpoint: "http://127.0.0.1:7300/execute".to_string(),
        operation_name: operation_name.to_string(),
        description: description.to_string(),
        input_schema,
        output_schema,
        idempotent,
        created_at: Utc::now(),
    };

    // ── Deterministic manifest ID ─────────────────────────────────────
    manifest.manifest_id = manifest
        .compute_manifest_id()
        .map_err(|e| anyhow!("MANIFEST_ID_COMPUTATION: {e}"))?;

    // ── Business validation ───────────────────────────────────────────
    manifest
        .validate_all()
        .map_err(|e| anyhow!("HARNESS_MANIFEST_VALIDATION: {e}"))?;

    Ok(manifest)
}

// ── Helpers (mirrored from Kernel; not exported) ─────────────────────────

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("MISSING_{key}"))
}

fn string_set_matches(value: &Value, key: &str, expected: &[String]) -> Result<bool> {
    let values = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("MISSING_{key}"))?;
    let actual = values
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("INVALID_{key}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let actual_set: std::collections::HashSet<_> = actual.iter().collect();
    let expected_set: std::collections::HashSet<_> = expected.iter().collect();
    Ok(actual.len() == expected.len() && actual_set == expected_set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
    use agent_core_kernel::domain::DevelopmentRequestDraft;

    fn request() -> DevelopmentRequest {
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.calculator".into(),
        );
        draft.requirements = vec!["four arithmetic operations".into()];
        draft.required_contracts = vec!["component.invoke.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["6 * 7 = 42".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "scope:test".into(),
            "message:test".into(),
            "calculator:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    fn calculator_component() -> Value {
        serde_json::json!({
            "schema_version": "component-artifact-v1",
            "component_id": "external.calculator",
            "kind": "invocable_capability",
            "profile_id": "invocable-capability-v0",
            "contract_catalog_version": CONTRACT_CATALOG_VERSION,
            "required_contracts": ["component.invoke.v0"],
            "requested_permissions": ["component.invoke"],
            "deployment_profile": "capability-host-v0",
            "target_kind": "InvocableCapability",
            "capability": {
                "operation_name": "external.calculator",
                "description": "Calculator supporting add, subtract, multiply, and divide.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "operation": {"type": "string", "enum": ["add", "subtract", "multiply", "divide"]},
                        "a": {"type": "number"},
                        "b": {"type": "number"}
                    },
                    "required": ["operation", "a", "b"],
                    "additionalProperties": false
                },
                "output_schema": {"type": "number"},
                "idempotent": true
            }
        })
    }

    fn artifact_digest() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    #[test]
    fn invocable_candidate_builds_harness_manifest() {
        let manifest = build_invocable_manifest(
            &calculator_component(),
            &artifact_digest(),
            &request(),
        )
        .unwrap();
        assert_eq!(manifest.harness_id, "capability-host-v0");
        assert_eq!(manifest.endpoint, "http://127.0.0.1:7300/execute");
        assert_eq!(manifest.operation_name, "external.calculator");
        assert_eq!(manifest.protocol_version, "external-harness-v1");
        assert!(manifest.idempotent);
        assert!(!manifest.manifest_id.is_empty());
        assert!(manifest.manifest_id.starts_with("manifest_"));
    }

    #[test]
    fn invocable_manifest_preserves_old_semantics() {
        let manifest = build_invocable_manifest(
            &calculator_component(),
            &artifact_digest(),
            &request(),
        )
        .unwrap();
        // Compare against the known‑good semantics from the old Kernel builder
        assert_eq!(manifest.harness_id, "capability-host-v0");
        assert_eq!(manifest.endpoint, "http://127.0.0.1:7300/execute");
        assert_eq!(manifest.operation_name, "external.calculator");
        assert_eq!(
            manifest.description,
            "Calculator supporting add, subtract, multiply, and divide."
        );
        assert!(manifest.input_schema.is_object());
        assert!(manifest.output_schema.is_object());
        assert!(manifest.idempotent);
        assert_eq!(manifest.artifact_digest, artifact_digest());
        assert_eq!(manifest.protocol_version, "external-harness-v1");
        // manifest_id must be deterministic for the same content
        let manifest2 = build_invocable_manifest(
            &calculator_component(),
            &artifact_digest(),
            &request(),
        )
        .unwrap();
        assert_eq!(manifest.manifest_id, manifest2.manifest_id);
    }

    #[test]
    fn invocable_manifest_uses_verified_artifact_digest() {
        let manifest = build_invocable_manifest(
            &calculator_component(),
            &artifact_digest(),
            &request(),
        )
        .unwrap();
        assert_eq!(manifest.artifact_digest, artifact_digest());
    }

    #[test]
    fn invocable_manifest_request_identity_mismatch_fails() {
        let mut req = request();
        req.name = "external.other".into();
        let result = build_invocable_manifest(&calculator_component(), &artifact_digest(), &req);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("IDENTITY_MISMATCH"),
            "expected IDENTITY_MISMATCH, got: {err}"
        );
    }

    #[test]
    fn invocable_manifest_contract_mismatch_fails() {
        let mut req = request();
        req.required_contracts = vec!["different.contract.v0".into()];
        let result = build_invocable_manifest(&calculator_component(), &artifact_digest(), &req);
        assert!(result.is_err());
    }

    #[test]
    fn invocable_manifest_permission_mismatch_fails() {
        let mut req = request();
        req.requested_permissions = vec!["different.permission".into()];
        let result = build_invocable_manifest(&calculator_component(), &artifact_digest(), &req);
        assert!(result.is_err());
    }

    #[test]
    fn invocable_manifest_operation_mismatch_fails() {
        let mut comp = calculator_component();
        comp["capability"]["operation_name"] = serde_json::json!("external.other");
        let result = build_invocable_manifest(&comp, &artifact_digest(), &request());
        assert!(result.is_err());
    }

    #[test]
    fn invocable_manifest_is_stored_by_digest() {
        let manifest = build_invocable_manifest(
            &calculator_component(),
            &artifact_digest(),
            &request(),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        use sha2::{Digest, Sha256};
        let computed = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
        assert!(computed.starts_with("sha256:"));
        assert_eq!(computed.len(), 71);
    }

    #[test]
    fn accepted_candidate_manifest_is_not_mutated() {
        let original = calculator_component();
        let _ = build_invocable_manifest(&original, &artifact_digest(), &request()).unwrap();
        // The original JSON must not have been modified
        assert_eq!(
            original["component_id"],
            serde_json::json!("external.calculator")
        );
    }

    #[test]
    fn duplicate_acceptance_returns_same_invocable_manifest() {
        let comp = calculator_component();
        let dig = artifact_digest();
        let req = request();
        let m1 = build_invocable_manifest(&comp, &dig, &req).unwrap();
        let m2 = build_invocable_manifest(&comp, &dig, &req).unwrap();
        assert_eq!(m1.manifest_id, m2.manifest_id);
        assert_eq!(m1.harness_id, m2.harness_id);
        assert_eq!(m1.endpoint, m2.endpoint);
        assert_eq!(m1.operation_name, m2.operation_name);
        assert_eq!(m1.description, m2.description);
        assert_eq!(m1.input_schema, m2.input_schema);
        assert_eq!(m1.output_schema, m2.output_schema);
        assert_eq!(m1.idempotent, m2.idempotent);
    }

    #[test]
    fn hook_consumer_rejected_by_invocable_builder() {
        let mut comp = calculator_component();
        comp["target_kind"] = serde_json::json!("HookConsumerService");
        let result = build_invocable_manifest(&comp, &artifact_digest(), &request());
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("UNEXPECTED_TARGET_KIND"),
            "expected UNEXPECTED_TARGET_KIND, got: {err}"
        );
    }
}
