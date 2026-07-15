use agent_core_kernel::contract_catalog::{ContractCatalog, CONTRACT_CATALOG_VERSION};
use agent_core_kernel::domain::TargetKind;
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::ComponentProfileCatalog;

#[derive(Debug, Clone)]
pub struct CandidateArtifactManifest {
    pub value: Value,
    pub component_id: String,
    pub target_kind: TargetKind,
    pub profile_id: String,
    pub test_kit: String,
    pub entry: PathBuf,
    pub artifact_digest: String,
}

impl CandidateArtifactManifest {
    pub fn load(candidate_root: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(candidate_root.join("manifest.json"))
            .map_err(|error| format!("manifest read error: {error}"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("manifest parse error: {error}"))?;
        Self::from_value(value)
    }

    pub fn from_value(value: Value) -> Result<Self, String> {
        exact(&value, "schema_version", "component-artifact-v1")?;
        exact(&value, "contract_catalog_version", CONTRACT_CATALOG_VERSION)?;
        let component_id = string(&value, "component_id")?;
        if !safe_id(&component_id) {
            return Err("component_id is not a safe identifier".into());
        }
        let target_kind: TargetKind = serde_json::from_value(
            value
                .get("kind")
                .cloned()
                .ok_or_else(|| "manifest missing kind".to_string())?,
        )
        .map_err(|_| "manifest kind is unsupported".to_string())?;
        let profile_id = string(&value, "profile_id")?;
        let test_kit = string(&value, "test_kit")?;
        let entry = PathBuf::from(string(&value, "entry")?);
        if entry.is_absolute() || entry.components().any(|part| part.as_os_str() == "..") {
            return Err("manifest entry path escapes candidate".into());
        }
        let artifact_digest = string(&value, "artifact_digest")?;
        if !valid_sha256_or_placeholder(&artifact_digest) {
            return Err("manifest artifact_digest is invalid".into());
        }

        let required_contracts = strings(&value, "required_contracts")?;
        let requested_permissions = strings(&value, "requested_permissions")?;
        let profiles = ComponentProfileCatalog::v1();
        let profile = profiles
            .get(&profile_id)
            .ok_or_else(|| "manifest profile_id is unknown".to_string())?;
        if !profile.target_kinds.contains(&target_kind) {
            return Err("manifest kind/profile mismatch".into());
        }
        let contracts = ContractCatalog::v1();
        for contract_id in required_contracts {
            let contract = contracts
                .get(&contract_id)
                .ok_or_else(|| format!("unknown required contract: {contract_id}"))?;
            if !profile.supported_contracts.contains(&contract_id) {
                return Err(format!("profile does not support contract: {contract_id}"));
            }
            for permission in &contract.permissions {
                if !requested_permissions.contains(permission) {
                    return Err(format!("missing contract permission: {permission}"));
                }
            }
        }
        for permission in requested_permissions {
            if !profile.permissions.contains(&permission) {
                return Err(format!("profile does not allow permission: {permission}"));
            }
        }
        crate::fixtures::validate_manifest(&test_kit, &value)?;

        Ok(Self {
            value,
            component_id,
            target_kind,
            profile_id,
            test_kit,
            entry,
            artifact_digest,
        })
    }
}

fn exact(value: &Value, key: &str, expected: &str) -> Result<(), String> {
    let actual = string(value, key)?;
    if actual != expected {
        return Err(format!("manifest {key} mismatch: {actual}"));
    }
    Ok(())
}

fn string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("manifest missing {key}"))
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>, String> {
    let values = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("manifest missing {key}"))?;
    if values.is_empty() {
        return Err(format!("manifest {key} is empty"));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("manifest {key} has invalid entry"))
        })
        .collect()
}

fn safe_id(value: &str) -> bool {
    value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn valid_sha256_or_placeholder(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}
