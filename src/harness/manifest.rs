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
/// PR #160 only supports this strict recursive subset:
/// - `type`: object, string, number, integer, boolean, array
/// - `properties` (object): maps name → schema
/// - `required` (object): array of required property names
/// - `additionalProperties` (object): boolean only (not schema object)
/// - `items` (array): single schema object
/// - `description`: informational, not validated at runtime
const ALLOWED_SCHEMA_KEYS: &[&str] = &[
    "type",
    "properties",
    "required",
    "items",
    "additionalProperties",
    "description",
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
    // Recursive subset validation: check this level and all nested schemas.
    validate_schema_node(params, index)?;
    Ok(())
}

/// Recursively validate a JSON Schema node against the allowed PR #160
/// subset. Rejects unsupported types, unsupported keywords, and invalid
/// `additionalProperties` / `items` values at every nesting level.
fn validate_schema_node(node: &serde_json::Value, index: usize) -> Result<()> {
    let obj = match node.as_object() {
        Some(o) => o,
        None => {
            return Err(anyhow!(
            "manifest_invalid: operation[{index}] parameters: schema node must be a JSON object"
        ))
        }
    };

    // Check type if present.
    if let Some(type_val) = obj.get("type") {
        let type_str = type_val.as_str().ok_or_else(|| {
            anyhow!("manifest_invalid: operation[{index}] parameters: 'type' must be a string")
        })?;
        match type_str {
            "object" | "string" | "number" | "integer" | "boolean" | "array" => {}
            other => {
                bail!("manifest_invalid: operation[{index}] parameters: unsupported type '{other}'")
            }
        }
    }

    // Reject unknown keys.
    for key in obj.keys() {
        if !ALLOWED_SCHEMA_KEYS.contains(&key.as_str()) {
            bail!(
                "manifest_invalid: operation[{index}] parameters: unsupported schema keyword '{key}'"
            );
        }
    }

    // Check required fields exist in properties.
    if let Some(required) = obj.get("required").and_then(|v| v.as_array()) {
        let props = obj.get("properties").and_then(|v| v.as_object());
        for req in required {
            let name = req.as_str().ok_or_else(|| anyhow!(
                "manifest_invalid: operation[{index}] parameters: 'required' entries must be strings"
            ))?;
            if props.map_or(true, |p| !p.contains_key(name)) {
                bail!(
                    "manifest_invalid: operation[{index}] parameters: required field '{name}' not found in properties"
                );
            }
        }
    }

    // Check additionalProperties is boolean only.
    if let Some(ap) = obj.get("additionalProperties") {
        if !ap.is_boolean() {
            bail!(
                "manifest_invalid: operation[{index}] parameters: 'additionalProperties' must be boolean"
            );
        }
    }

    // Recursively validate properties.
    if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
        for prop_schema in props.values() {
            validate_schema_node(prop_schema, index)?;
        }
    }

    // Recursively validate items (must be a single schema object).
    if let Some(items) = obj.get("items") {
        if !items.is_object() {
            bail!(
                "manifest_invalid: operation[{index}] parameters: 'items' must be a JSON object schema"
            );
        }
        validate_schema_node(items, index)?;
    }

    Ok(())
}

// ---- Canonical Manifest ----

/// Produce a deterministic canonical Value from a validated manifest.
/// The result has:
/// - Operations sorted by name
/// - Parameters are re-serialized for canonical key ordering
/// - No `bundle_hash` or other runtime fields
///
/// This single value feeds into both `compute_bundle_hash` and
/// `manifest_json` storage, guaranteeing byte-level consistency.
pub fn canonical_manifest_value(manifest: &HarnessBundleManifest) -> serde_json::Value {
    let mut ops = Vec::with_capacity(manifest.operations.len());
    let mut sorted = manifest.operations.clone();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for op in &sorted {
        // Re-serialize parameters to get canonical key ordering.
        let canonical_params =
            canonicalize_json(&op.parameters).unwrap_or_else(|_| op.parameters.clone());
        let map = serde_json::json!({
            "name": op.name,
            "description": op.description,
            "parameters": canonical_params,
            "risk": op.risk,
            "idempotent": op.idempotent,
        });
        ops.push(map);
    }
    serde_json::json!({
        "manifest_version": manifest.manifest_version,
        "protocol_version": manifest.protocol_version,
        "bundle_id": manifest.bundle_id,
        "bundle_version": manifest.bundle_version,
        "operations": ops,
    })
}

/// Canonicalize a JSON value by recursively sorting object keys.
fn canonicalize_json(value: &serde_json::Value) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                sorted.insert(key.clone(), canonicalize_json(&map[key])?);
            }
            Ok(serde_json::Value::Object(sorted))
        }
        serde_json::Value::Array(arr) => {
            let vec: Result<Vec<_>> = arr.iter().map(canonicalize_json).collect();
            Ok(serde_json::Value::Array(vec?))
        }
        other => Ok(other.clone()),
    }
}

// ---- Bundle Hash ----

/// Compute the deterministic bundle hash from the canonical manifest value.
/// The hash is always over the same canonical JSON that is persisted as
/// `manifest_json`, ensuring byte-level consistency.
pub fn compute_bundle_hash(manifest: &HarnessBundleManifest) -> String {
    let canonical = canonical_manifest_value(manifest);
    let json = serde_json::to_string(&canonical).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
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

fn is_valid_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.' || c == '-' || c.is_ascii_digit())
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
