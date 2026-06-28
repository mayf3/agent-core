//! Harness bundle manifest registration, validation, and hash computation.
//!
//! PR 2A: immutable bundle manifest registration. Manifests are stored
//! append-only in `harness_bundles`. The bundle hash is a deterministic
//! SHA-256 of the canonical manifest fields (excluding runtime state).

use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---- Constants ----

pub const MAX_MANIFEST_BYTES: usize = 256 * 1024; // 256 KB
pub const MAX_OPERATIONS: usize = 64;
pub const MAX_OP_NAME_LEN: usize = 128;
pub const MAX_DESCRIPTION_LEN: usize = 1000;
pub const MAX_PARAMETERS_BYTES: usize = 64 * 1024; // 64 KB

/// Allowed JSON Schema keys in the parameters for v1 external harness.
const ALLOWED_SCHEMA_KEYS: &[&str] = &[
    "type",
    "properties",
    "required",
    "items",
    "additionalProperties",
    "description",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "enum",
];

/// Supported parameter types for argument validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaType {
    String,
    Number,
    Integer,
    Boolean,
    Object,
    Array,
}

// ---- Data Types ----

/// The manifest as received from an operator registering an external harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessBundleManifest {
    pub manifest_version: String,
    pub protocol_version: String,
    pub bundle_id: String,
    pub bundle_version: String,
    pub operations: Vec<HarnessManifestOperation>,
}

/// A single operation declared in a harness manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessManifestOperation {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub risk: String,
    pub idempotent: bool,
}

/// A validated operation ready for snapshot use, with computed binding.
#[derive(Debug, Clone)]
pub struct PreparedOperation {
    pub spec: OperationSpec,
    pub bundle_hash: String,
}

// ---- Manifest Validation ----

/// Validate a raw manifest JSON against all PR 2A constraints.
/// Returns the parsed manifest on success.
pub fn validate_manifest(
    raw: &serde_json::Value,
    declared_hash: Option<&str>,
) -> Result<HarnessBundleManifest> {
    let canonical = canonicalize_json(raw)?;
    #[allow(unused_variables)]
    let manifest: HarnessBundleManifest = serde_json::from_value(canonical.clone())
        .map_err(|e| anyhow!("manifest_invalid: parse error: {e}"))?;

    // Version constraints.
    if manifest.manifest_version != "v1" {
        bail!(
            "manifest_invalid: manifest_version must be v1, got {}",
            manifest.manifest_version
        );
    }
    if manifest.protocol_version != "v1" {
        bail!(
            "manifest_invalid: protocol_version must be v1, got {}",
            manifest.protocol_version
        );
    }

    // Bundle identity.
    if manifest.bundle_id.is_empty() || !is_valid_name(&manifest.bundle_id) {
        bail!("manifest_invalid: bundle_id is empty or contains illegal characters");
    }
    if manifest.bundle_version.is_empty() {
        bail!("manifest_invalid: bundle_version must not be empty");
    }

    // Operations.
    if manifest.operations.is_empty() {
        bail!("manifest_invalid: operations must not be empty");
    }
    if manifest.operations.len() > MAX_OPERATIONS {
        bail!(
            "manifest_invalid: too many operations: {} > max {MAX_OPERATIONS}",
            manifest.operations.len()
        );
    }

    let mut names_seen = std::collections::HashSet::new();
    for (i, op) in manifest.operations.iter().enumerate() {
        validate_operation(op, i)?;
        if !names_seen.insert(&op.name) {
            bail!("duplicate operation name: {}", op.name);
        }
    }

    // Compute bundle hash.
    let hash = compute_bundle_hash(&manifest);
    if let Some(declared) = declared_hash {
        if declared != hash {
            bail!(
                "declared bundle hash {} does not match computed hash {}",
                declared,
                hash
            );
        }
    }

    Ok(manifest)
}

fn validate_operation(op: &HarnessManifestOperation, index: usize) -> Result<()> {
    if op.name.is_empty() || op.name.len() > MAX_OP_NAME_LEN {
        bail!(
            "manifest_invalid: operation[{}] name empty or > {MAX_OP_NAME_LEN}",
            index
        );
    }
    if !is_valid_name(&op.name) {
        bail!(
            "manifest_invalid: operation[{}] name '{}' contains illegal characters",
            index,
            op.name
        );
    }
    if op.description.is_empty() || op.description.len() > MAX_DESCRIPTION_LEN {
        bail!(
            "manifest_invalid: operation[{}] description empty or > {MAX_DESCRIPTION_LEN}",
            index
        );
    }
    if op.risk != "ReadOnly" {
        bail!(
            "manifest_invalid: operation[{}] risk must be ReadOnly for v1 harness, got '{}'",
            index,
            op.risk
        );
    }
    if !op.idempotent {
        bail!(
            "manifest_invalid: operation[{}] idempotent must be true for v1 harness",
            index
        );
    }
    validate_parameters(&op.parameters, index)?;
    Ok(())
}

fn validate_parameters(params: &serde_json::Value, index: usize) -> Result<()> {
    let serialized = serde_json::to_string(params)?;
    if serialized.len() > MAX_PARAMETERS_BYTES {
        bail!(
            "manifest_invalid: operation[{}] parameters too large",
            index
        );
    }
    let obj = params.as_object().ok_or_else(|| {
        anyhow!(
            "manifest_invalid: operation[{}] parameters must be a JSON object",
            index
        )
    })?;
    for key in obj.keys() {
        if !ALLOWED_SCHEMA_KEYS.contains(&key.as_str()) {
            bail!(
                "manifest_invalid: operation[{}] unsupported schema key '{key}' (allowed: {:?})",
                index,
                ALLOWED_SCHEMA_KEYS
            );
        }
    }
    Ok(())
}

// ---- Bundle Hash ----

/// Compute the deterministic bundle hash.
pub fn compute_bundle_hash(manifest: &HarnessBundleManifest) -> String {
    let mut sorted = manifest.operations.clone();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));

    let mut map = serde_json::Map::new();
    map.insert(
        "manifest_version".to_string(),
        serde_json::json!(manifest.manifest_version),
    );
    map.insert(
        "protocol_version".to_string(),
        serde_json::json!(manifest.protocol_version),
    );
    map.insert(
        "bundle_id".to_string(),
        serde_json::json!(manifest.bundle_id),
    );
    map.insert(
        "bundle_version".to_string(),
        serde_json::json!(manifest.bundle_version),
    );

    let ops: Vec<serde_json::Value> = sorted
        .iter()
        .map(|op| {
            let mut om = serde_json::Map::new();
            om.insert("name".to_string(), serde_json::json!(op.name));
            om.insert("description".to_string(), serde_json::json!(op.description));
            om.insert(
                "parameters".to_string(),
                canonicalize_json(&op.parameters).unwrap_or(op.parameters.clone()),
            );
            om.insert("risk".to_string(), serde_json::json!(op.risk));
            om.insert("idempotent".to_string(), serde_json::json!(op.idempotent));
            serde_json::Value::Object(om)
        })
        .collect();
    map.insert("operations".to_string(), serde_json::json!(ops));

    let canonical = serde_json::to_string(&serde_json::Value::Object(map)).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Prepare an OperationSpec from a manifest operation + bundle hash.
pub fn prepare_operation(op: &HarnessManifestOperation, bundle_hash: &str) -> PreparedOperation {
    PreparedOperation {
        bundle_hash: bundle_hash.to_string(),
        spec: OperationSpec {
            name: op.name.clone(),
            risk: Risk::ReadOnly,
            description: op.description.clone(),
            parameters: op.parameters.clone(),
            idempotent: true,
            binding_kind: BindingKind::ExternalHarness,
            binding_key: format!("harness:{bundle_hash}:{}", op.name),
        },
    }
}

// ---- Helpers ----

fn canonicalize_json(value: &serde_json::Value) -> Result<serde_json::Value> {
    let serialized = serde_json::to_string(value)?;
    Ok(serde_json::from_str(&serialized)?)
}

fn is_valid_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.' || c == '-' || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> serde_json::Value {
        serde_json::json!({
            "manifest_version": "v1",
            "protocol_version": "v1",
            "bundle_id": "my_harness",
            "bundle_version": "1.0.0",
            "operations": [{
                "name": "my_op",
                "description": "my harness operation",
                "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": false},
                "risk": "ReadOnly",
                "idempotent": true
            }]
        })
    }

    #[test]
    fn valid_manifest_passes_validation() {
        let result = validate_manifest(&valid_manifest(), None);
        assert!(result.is_ok(), "valid manifest: {:?}", result.err());
    }

    #[test]
    fn declared_hash_matches() {
        let manifest = validate_manifest(&valid_manifest(), None).unwrap();
        let hash = compute_bundle_hash(&manifest);
        let result = validate_manifest(&valid_manifest(), Some(&hash));
        assert!(result.is_ok());
    }

    #[test]
    fn declared_hash_mismatch_rejected() {
        let result = validate_manifest(&valid_manifest(), Some("sha256:badhash"));
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("declared bundle hash") || err.contains("does not match"));
    }

    #[test]
    fn manifest_version_must_be_v1() {
        let mut m = valid_manifest();
        m["manifest_version"] = serde_json::json!("v2");
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn protocol_version_must_be_v1() {
        let mut m = valid_manifest();
        m["protocol_version"] = serde_json::json!("v2");
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn operations_must_not_be_empty() {
        let mut m = valid_manifest();
        m["operations"] = serde_json::json!([]);
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn duplicate_operation_names_rejected() {
        let mut m = valid_manifest();
        m["operations"] = serde_json::json!([
            {"name": "my_op", "description": "first", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
            {"name": "my_op", "description": "second", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
        ]);
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn risk_must_be_readonly() {
        let mut m = valid_manifest();
        m["operations"][0]["risk"] = serde_json::json!("Write");
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn idempotent_must_be_true() {
        let mut m = valid_manifest();
        m["operations"][0]["idempotent"] = serde_json::json!(false);
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn operation_name_illegal_chars_rejected() {
        let mut m = valid_manifest();
        m["operations"][0]["name"] = serde_json::json!("illegal space");
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn bundle_id_must_be_valid() {
        let mut m = valid_manifest();
        m["bundle_id"] = serde_json::json!("");
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn bundle_version_must_not_be_empty() {
        let mut m = valid_manifest();
        m["bundle_version"] = serde_json::json!("");
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn unsupported_schema_keyword_rejected() {
        let mut m = valid_manifest();
        m["operations"][0]["parameters"] =
            serde_json::json!({"type": "object", "$ref": "#/definitions/X"});
        assert!(validate_manifest(&m, None).is_err());
    }

    #[test]
    fn canonicalization_normalizes_json_order() {
        let m1 = serde_json::json!({
            "manifest_version": "v1",
            "protocol_version": "v1",
            "bundle_id": "harness",
            "bundle_version": "1.0",
            "operations": [{"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
        });
        let m2 = serde_json::json!({
            "protocol_version": "v1",
            "bundle_id": "harness",
            "bundle_version": "1.0",
            "manifest_version": "v1",
            "operations": [{"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
        });
        let manifest1 = validate_manifest(&m1, None).unwrap();
        let manifest2 = validate_manifest(&m2, None).unwrap();
        let hash1 = compute_bundle_hash(&manifest1);
        let hash2 = compute_bundle_hash(&manifest2);
        assert_eq!(hash1, hash2, "different key order must produce same hash");
    }

    #[test]
    fn operations_order_does_not_affect_hash() {
        let m1 = serde_json::json!({
            "manifest_version": "v1",
            "protocol_version": "v1",
            "bundle_id": "harness",
            "bundle_version": "1.0",
            "operations": [
                {"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
                {"name": "op_b", "description": "b", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}
            ]
        });
        let m2 = serde_json::json!({
            "manifest_version": "v1",
            "protocol_version": "v1",
            "bundle_id": "harness",
            "bundle_version": "1.0",
            "operations": [
                {"name": "op_b", "description": "b", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
                {"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}
            ]
        });
        let manifest1 = validate_manifest(&m1, None).unwrap();
        let manifest2 = validate_manifest(&m2, None).unwrap();
        let hash1 = compute_bundle_hash(&manifest1);
        let hash2 = compute_bundle_hash(&manifest2);
        assert_eq!(
            hash1, hash2,
            "different operation order must produce same hash"
        );
    }

    #[test]
    fn hash_is_deterministic() {
        let manifest = validate_manifest(&valid_manifest(), None).unwrap();
        let h1 = compute_bundle_hash(&manifest);
        let h2 = compute_bundle_hash(&manifest);
        assert_eq!(h1, h2);
    }

    #[test]
    fn prepare_operation_sets_external_harness_binding() {
        let manifest = validate_manifest(&valid_manifest(), None).unwrap();
        let hash = compute_bundle_hash(&manifest);
        let prepared = prepare_operation(&manifest.operations[0], &hash);
        assert_eq!(prepared.spec.binding_kind, BindingKind::ExternalHarness);
        assert!(prepared.spec.binding_key.starts_with("harness:"));
        assert!(prepared.spec.binding_key.contains(&hash));
    }

    #[test]
    fn declared_hash_stripped_by_canonicalization() {
        // declared_hash is consumed by validation, not stored in manifest
        let manifest = validate_manifest(&valid_manifest(), None).unwrap();
        assert!(serde_json::to_string(&manifest).unwrap().contains("my_op"));
        // The canonical manifest should not contain 'bundle_hash' field
        let canonical = serde_json::to_string(&manifest).unwrap();
        assert!(
            !canonical.contains("bundle_hash"),
            "bundle_hash should not be in canonical manifest"
        );
    }
}
