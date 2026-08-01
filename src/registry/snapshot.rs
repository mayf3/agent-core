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

/// The contract name of a hook binding. A `HookBinding` is selected by its
/// `contract` — the Kernel resolves the exact binding for a lifecycle point
/// and fails closed when the contract is missing or duplicated.
pub const BUDGET_HOOK_CONTRACT: &str = "run.budget.resolve.v0";

/// A generic hook binding frozen into a Registry Snapshot. One binding per
/// `contract` (enforced at creation); a snapshot without the binding a
/// lifecycle point requires fails closed for new Runs.
///
/// `provider_id` / `endpoint` are transport facts owned by the snapshot.
/// The shared secret is NEVER stored here — it lives in the Kernel's local
/// config (env) keyed by `provider_id`, so a snapshot can't leak credentials
/// and env changes can't silently switch hook identity.
///
/// New hook contracts (e.g. `context.prepare.v0`) can be added as plain data
/// values without changing this struct or the database schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookBinding {
    /// The hook contract this binding serves, e.g. `run.budget.resolve.v0`.
    pub contract: String,
    /// Stable hook identity, e.g. `builtin:run-budget-default-v0`.
    pub hook_id: String,
    /// Hook contract version, e.g. `v0`.
    pub hook_version: String,
    /// How the hook is implemented. `Builtin` = the Kernel's default decision
    /// function; `External` = an HTTP endpoint resolved from `endpoint`.
    pub binding_kind: BindingKind,
    /// Stable handler key (never a function pointer or process handle).
    pub binding_key: String,
    /// Trusted identity for an External binding; empty for Builtin.
    pub provider_id: String,
    /// HTTP(S) endpoint for an External binding; empty for Builtin.
    pub endpoint: String,
}

impl HookBinding {
    /// The canonical budget hook binding registered in every bootstrap
    /// snapshot. The Kernel's default decision function is the `Builtin`
    /// implementation of this binding — an ordinary registered artifact,
    /// not a parallel strategy path.
    pub fn builtin_budget() -> Self {
        Self {
            contract: BUDGET_HOOK_CONTRACT.to_string(),
            hook_id: "builtin:run-budget-default-v0".to_string(),
            hook_version: "v0".to_string(),
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.run_budget_default".to_string(),
            provider_id: String::new(),
            endpoint: String::new(),
        }
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
    /// The generic hook bindings frozen into this snapshot, one per contract.
    /// The Kernel resolves the binding for a lifecycle point by `contract` and
    /// fails closed when the contract is missing or duplicated.
    #[serde(default)]
    pub hook_bindings: Vec<HookBinding>,
}

impl RegistrySnapshot {
    /// An empty snapshot for testing edge cases.
    pub fn empty() -> Self {
        Self {
            snapshot_id: String::new(),
            created_at: Utc::now(),
            operations: vec![],
            hook_bindings: vec![],
        }
    }

    /// Resolve the unique binding for a hook contract. Fails closed when the
    /// contract is missing or has more than one binding.
    pub fn hook_binding(&self, contract: &str) -> Option<&HookBinding> {
        let matches: Vec<&HookBinding> = self
            .hook_bindings
            .iter()
            .filter(|b| b.contract == contract)
            .collect();
        match matches.len() {
            1 => Some(matches[0]),
            _ => None,
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

/// Compute a deterministic snapshot ID from the operation specs and the
/// snapshot's budget hook binding. The input is canonicalized: operations
/// sorted by name, using a deterministic JSON representation that excludes
/// `created_at`, memory addresses, and random values. Two spec sets with the
/// same operations AND the same hook bindings produce the same ID — changing
/// any binding (or adding a new contract) produces a new snapshot ID. The
/// binding set is canonicalized by sorting on `contract`, so storage order
/// never affects the digest.
pub fn compute_snapshot_id_with_hook_bindings(
    specs: &[OperationSpec],
    hook_bindings: &[HookBinding],
) -> Result<String> {
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
    // Generic hook bindings: sorted by contract, included under a stable key.
    let mut sorted_bindings: Vec<&HookBinding> = hook_bindings.iter().collect();
    sorted_bindings.sort_by(|a, b| a.contract.cmp(&b.contract));
    if !sorted_bindings.is_empty() {
        canonical.insert(
            "__hook_bindings".to_string(),
            serde_json::Value::Array(
                sorted_bindings
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "contract": b.contract,
                            "hook_id": b.hook_id,
                            "hook_version": b.hook_version,
                            "binding_kind": format!("{:?}", b.binding_kind),
                            "binding_key": b.binding_key,
                            "provider_id": b.provider_id,
                            "endpoint": b.endpoint,
                        })
                    })
                    .collect(),
            ),
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

/// Backward-compatible wrapper: computes the snapshot ID without hook
/// bindings. Kept for call sites that predate the binding field; all
/// Registry creation paths must use `compute_snapshot_id_with_hook_bindings`.
pub fn compute_snapshot_id(specs: &[OperationSpec]) -> Result<String> {
    compute_snapshot_id_with_hook_bindings(specs, &[])
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
        hook_bindings: vec![HookBinding::builtin_budget()],
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
            hook_bindings: vec![],
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
            hook_bindings: vec![],
        };
        let text = snap.catalog_for_context_grants(&["system.status".to_string()]);
        assert!(text.contains("system.status"));
        let empty = snap.catalog_for_context_grants(&[]);
        assert!(empty.contains("No tools"));
    }
}
