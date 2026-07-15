use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::TargetKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    Healthy,
    Disabled,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredComponent {
    pub component_id: String,
    pub kind: TargetKind,
    pub manifest_id: String,
    pub manifest_digest: String,
    pub artifact_digest: String,
    pub version: String,
    pub endpoint: String,
    pub deployment_id: String,
    pub deployment_receipt_id: String,
    pub status: ComponentStatus,
    pub required_contracts: Vec<String>,
    pub requested_permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentRegistrySnapshot {
    pub snapshot_id: String,
    pub created_at: DateTime<Utc>,
    pub components: Vec<RegisteredComponent>,
}

impl ComponentRegistrySnapshot {
    pub fn lookup(&self, component_id: &str) -> Option<&RegisteredComponent> {
        self.components
            .iter()
            .find(|component| component.component_id == component_id)
    }
}

pub fn compute_component_snapshot_id(components: &[RegisteredComponent]) -> Result<String> {
    let mut canonical = BTreeMap::new();
    for component in components {
        canonical.insert(component.component_id.clone(), component);
    }
    let digest = Sha256::digest(serde_json::to_vec(&canonical)?);
    Ok(format!("component_snap_{}", hex::encode(digest)))
}
