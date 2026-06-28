use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The risk classification of an operation. `Write` operations use the
/// approval/dispatch boundary; catalogued `ReadOnly` operations may execute
/// inline after the Gateway approves the current run's explicit grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Risk {
    ReadOnly,
    Write,
}

/// How an operation is implemented. PR 1 supports only `builtin`; future PRs
/// may add `external` (Harness adapter). This is persisted, so it must remain
/// stable and cheap to serialize — never store a function pointer, endpoint,
/// or process handle here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingKind {
    Builtin,
    ExternalHarness,
}

/// A known operation — runtime-owned (no `&'static str`). The `parameters`
/// field is the full JSON schema sent to the provider, so new operations no
/// longer need a hardcoded match arm in `provider_tool_definition`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationSpec {
    pub name: String,
    pub risk: Risk,
    pub description: String,
    pub parameters: serde_json::Value,
    pub idempotent: bool,
    /// The implementation binding for this operation.
    pub binding_kind: BindingKind,
    /// A stable key identifying the built-in handler (e.g. `builtin.time_now`).
    /// Never a function pointer or runtime endpoint.
    pub binding_key: String,
}

impl OperationSpec {
    /// The OpenAI-compatible tool definition for the provider. Returns `None`
    /// for Write operations (they are never sent as tools).
    pub fn provider_tool_definition(&self) -> Option<serde_json::Value> {
        if self.risk != Risk::ReadOnly {
            return None;
        }
        Some(serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        }))
    }
}

/// An immutable snapshot of the operation registry at a point in time. Each Run
/// pins to one snapshot; Context, Provider tools, and Gateway validation all
/// read from that pinned snapshot, so activating a new version mid-Run does not
/// affect in-flight Runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub snapshot_id: String,
    pub created_at: DateTime<Utc>,
    pub operations: Vec<OperationSpec>,
}

impl RegistrySnapshot {
    /// An empty snapshot for testing edge cases.
    pub fn empty() -> Self {
        Self {
            snapshot_id: String::new(),
            created_at: Utc::now(),
            operations: vec![],
        }
    }
    /// Look up an operation by name.
    pub fn lookup(&self, name: &str) -> Option<&OperationSpec> {
        self.operations.iter().find(|op| op.name == name)
    }

    /// Provider tools for the granted operations, filtered to ReadOnly, in
    /// snapshot order. This replaces the static `provider_tools_for_grants`.
    pub fn provider_tools_for_grants(&self, granted: &[String]) -> Vec<serde_json::Value> {
        self.operations
            .iter()
            .filter(|op| op.risk == Risk::ReadOnly && granted.iter().any(|g| g == &op.name))
            .filter_map(|op| op.provider_tool_definition())
            .collect()
    }

    /// ToolCatalog text for the Context block, from this snapshot's granted
    /// ReadOnly operations. Replaces the static `catalog_for_context_grants`.
    pub fn catalog_for_context_grants(&self, granted: &[String]) -> String {
        let names: Vec<&str> = self
            .operations
            .iter()
            .filter(|op| op.risk == Risk::ReadOnly && granted.iter().any(|g| g == &op.name))
            .map(|op| op.name.as_str())
            .collect();
        if names.is_empty() {
            return "No tools are available for this request.".to_string();
        }
        let rows = names
            .iter()
            .map(|name| format!("{name} - {}", self.description_for(name)))
            .collect::<Vec<_>>()
            .join("\n");
        format!("Available tools (authorized for this request, read-only):\n{rows}")
    }

    fn description_for(&self, name: &str) -> &str {
        self.operations
            .iter()
            .find(|op| op.name == name)
            .map(|op| op.description.as_str())
            .unwrap_or("catalogued read-only operation.")
    }
}

/// Compute a deterministic snapshot ID from the operation specs.
/// The input is canonicalized: operations sorted by name, using a deterministic
/// JSON representation that excludes `created_at`, memory addresses, and random
/// values. Two spec sets with the same operations produce the same ID.
pub fn compute_snapshot_id(specs: &[OperationSpec]) -> Result<String> {
    let mut sorted: Vec<&OperationSpec> = specs.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut canonical: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for spec in &sorted {
        canonical.insert(
            spec.name.clone(),
            serde_json::json!({
                "risk": format!("{:?}", spec.risk),
                "description": spec.description,
                "parameters": spec.parameters,
                "idempotent": spec.idempotent,
                "binding_kind": format!("{:?}", spec.binding_kind),
                "binding_key": spec.binding_key,
            }),
        );
    }
    let canonical_json = serde_json::to_string(&serde_json::Value::Object(
        canonical
            .into_iter()
            .map(|(k, v)| (k, v))
            .collect::<serde_json::Map<_, _>>(),
    ))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical_json.as_bytes());
    let digest = hex::encode(hasher.finalize());
    Ok(format!("snap_{digest}"))
}

/// Build a test snapshot from the builtin specs. Available in all build profiles
/// so integration tests can use it without constructing one manually.
pub fn test_snapshot() -> RegistrySnapshot {
    use crate::registry::store::builtin_specs;
    let operations = builtin_specs();
    RegistrySnapshot {
        snapshot_id: "snap_test_default".to_string(),
        created_at: chrono::Utc::now(),
        operations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, risk: Risk) -> OperationSpec {
        OperationSpec {
            name: name.into(),
            risk,
            description: "test".into(),
            parameters: serde_json::json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: format!("builtin.{name}"),
        }
    }

    #[test]
    fn same_specs_same_id_regardless_of_input_order() {
        let s1 = compute_snapshot_id(&[spec("b", Risk::ReadOnly), spec("a", Risk::Write)]).unwrap();
        let s2 = compute_snapshot_id(&[spec("a", Risk::Write), spec("b", Risk::ReadOnly)]).unwrap();
        assert_eq!(s1, s2, "order-independent");
    }

    #[test]
    fn different_risk_produces_different_id() {
        let id1 = compute_snapshot_id(&[spec("x", Risk::ReadOnly)]).unwrap();
        let id2 = compute_snapshot_id(&[spec("x", Risk::Write)]).unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn different_schema_produces_different_id() {
        let mut s = spec("x", Risk::ReadOnly);
        let id1 = compute_snapshot_id(&[s.clone()]).unwrap();
        s.parameters = serde_json::json!({"type": "string"});
        let id2 = compute_snapshot_id(&[s]).unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn snapshot_id_starts_with_snap_prefix() {
        let id = compute_snapshot_id(&[spec("x", Risk::ReadOnly)]).unwrap();
        assert!(id.starts_with("snap_"));
    }

    #[test]
    fn provider_tools_filters_write_and_unknown() {
        let snap = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: Utc::now(),
            operations: vec![
                spec("time.now", Risk::ReadOnly),
                spec("feishu.send_message", Risk::Write),
            ],
        };
        let tools = snap.provider_tools_for_grants(&[
            "time.now".to_string(),
            "feishu.send_message".to_string(),
        ]);
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].pointer("/function/name").and_then(|v| v.as_str()),
            Some("time.now")
        );
    }

    #[test]
    fn catalog_for_context_grants_from_snapshot() {
        let snap = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: Utc::now(),
            operations: vec![spec("time.now", Risk::ReadOnly)],
        };
        let text = snap.catalog_for_context_grants(&["time.now".to_string()]);
        assert!(text.contains("time.now"));
        let empty = snap.catalog_for_context_grants(&[]);
        assert!(empty.contains("No tools"));
    }
}
