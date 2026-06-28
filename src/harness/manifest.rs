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
    let manifest: HarnessBundleManifest = serde_json::from_value(canonical.clone())
        .map_err(|e| anyhow!("manifest parse error: {e}"))?;

    // Version constraints.
    if manifest.manifest_version != "v1" {
        bail!(
            "manifest_version must be v1, got {}",
            manifest.manifest_version
        );
    }
    if manifest.protocol_version != "v1" {
        bail!(
            "protocol_version must be v1, got {}",
            manifest.protocol_version
        );
    }

    // Bundle identity.
    if manifest.bundle_id.is_empty() || !is_valid_name(&manifest.bundle_id) {
        bail!("bundle_id is empty or contains illegal characters");
    }
    if manifest.bundle_version.is_empty() {
        bail!("bundle_version must not be empty");
    }

    // Operations.
    if manifest.operations.is_empty() {
        bail!("operations must not be empty");
    }
    if manifest.operations.len() > MAX_OPERATIONS {
        bail!(
            "too many operations: {} > max {MAX_OPERATIONS}",
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
        bail!("operation[{}] name empty or > {MAX_OP_NAME_LEN}", index);
    }
    if !is_valid_name(&op.name) {
        bail!(
            "operation[{}] name '{}' contains illegal characters",
            index,
            op.name
        );
    }
    if op.description.is_empty() || op.description.len() > MAX_DESCRIPTION_LEN {
        bail!(
            "operation[{}] description empty or > {MAX_DESCRIPTION_LEN}",
            index
        );
    }
    if op.risk != "ReadOnly" {
        bail!(
            "operation[{}] risk must be ReadOnly for v1 harness, got '{}'",
            index,
            op.risk
        );
    }
    if !op.idempotent {
        bail!(
            "operation[{}] idempotent must be true for v1 harness",
            index
        );
    }
    validate_parameters(&op.parameters, index)?;
    Ok(())
}

fn validate_parameters(params: &serde_json::Value, index: usize) -> Result<()> {
    let serialized = serde_json::to_string(params)?;
    if serialized.len() > MAX_PARAMETERS_BYTES {
        bail!("operation[{}] parameters too large", index);
    }
    let obj = params
        .as_object()
        .ok_or_else(|| anyhow!("operation[{}] parameters must be a JSON object", index))?;
    for key in obj.keys() {
        if !ALLOWED_SCHEMA_KEYS.contains(&key.as_str()) {
            bail!(
                "operation[{}] unsupported schema key '{key}' (allowed: {:?})",
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
