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

/// How an operation is implemented. PR 1 supports only `builtin`; PR 162 adds
/// `external` (Harness adapter). This is persisted, so it must remain
/// stable and cheap to serialize — never store a function pointer, endpoint,
/// or process handle here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingKind {
    Builtin,
    External,
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
    /// Whether this operation should appear in provider-facing tool definitions
    /// and the context tool catalog. ReadOnly operations are always included.
    /// External Write operations (e.g. coding-harness operations) are included
    /// so the model can call them when granted. Builtin Write operations are
    /// excluded — they are system-level (e.g. `feishu.send_message`) and should
    /// never appear in model-facing tools.
    pub fn is_visible_to_provider(&self) -> bool {
        match self.risk {
            Risk::ReadOnly => true,
            Risk::Write => self.binding_kind == BindingKind::External,
        }
    }

    /// The OpenAI-compatible tool definition for the provider.
    pub fn provider_tool_definition(&self) -> Option<serde_json::Value> {
        if !self.is_visible_to_provider() {
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

    /// Provider tools for the granted operations. ReadOnly operations and
    /// external Write operations (e.g. coding-harness operations) that have
    /// an explicit grant are included. Builtin Write operations are excluded.
    pub fn provider_tools_for_grants(&self, granted: &[String]) -> Vec<serde_json::Value> {
        self.operations
            .iter()
            .filter(|op| granted.iter().any(|g| g == &op.name))
            .filter_map(|op| op.provider_tool_definition())
            .collect()
    }

    /// ToolCatalog text for the Context block, from this snapshot's granted
    /// operations. Uses the same inclusion rules as provider_tools_for_grants.
    pub fn catalog_for_context_grants(&self, granted: &[String]) -> String {
        let names: Vec<&str> = self
            .operations
            .iter()
            .filter(|op| granted.iter().any(|g| g == &op.name) && op.is_visible_to_provider())
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
        format!("Available tools (authorized for this request):\n{rows}")
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
    use serde_json::json;

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
    fn provider_tools_includes_external_write_but_not_builtin_write() {
        let snap = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: Utc::now(),
            operations: vec![
                spec("system.status", Risk::ReadOnly),
                spec("feishu.send_message", Risk::Write),
                OperationSpec {
                    name: "external.coding_workspace_write".into(),
                    risk: Risk::Write,
                    description: "Write".into(),
                    parameters: json!({"type": "object"}),
                    idempotent: false,
                    binding_kind: BindingKind::External,
                    binding_key: "external.key".into(),
                },
            ],
        };
        let tools = snap.provider_tools_for_grants(&[
            "system.status".to_string(),
            "feishu.send_message".to_string(),
            "external.coding_workspace_write".to_string(),
        ]);
        // Builtin Write (feishu.send_message) is excluded; external Write
        // (coding_workspace_write) is included.
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"system.status"));
        assert!(names.contains(&"external.coding_workspace_write"));
        assert!(!names.contains(&"feishu.send_message"));
    }

    #[test]
    fn catalog_for_context_grants_from_snapshot() {
        let snap = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: Utc::now(),
            operations: vec![spec("system.status", Risk::ReadOnly)],
        };
        let text = snap.catalog_for_context_grants(&["system.status".to_string()]);
        assert!(text.contains("system.status"));
        let empty = snap.catalog_for_context_grants(&[]);
        assert!(empty.contains("No tools"));
    }
}
