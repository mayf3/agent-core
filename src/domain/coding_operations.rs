//! Coding-harness operation names and risk classification.
//!
//! Extracted from `operation.rs` to keep module sizes under the 500-line
//! structure limit. The `external` submodule and `coding_operation_risk`
//! are re-exported from `operation.rs` so all existing import paths work.

/// Known external (harness-registered) operation name constants.
pub mod external {
    pub const EXECUTION_LIST: &str = "external.execution_list";
    pub const EXECUTION_READ: &str = "external.execution_read";
    pub const EXECUTION_WRITE: &str = "external.execution_write";
    pub const EXECUTION_EXEC: &str = "external.execution_exec";
    pub const EXECUTION_TASK: &str = "external.execution_task";
    pub const TASK_SUBMIT: &str = "external.coding_task_submit";
    pub const TASK_STATUS: &str = "external.coding_task_status";
    pub const CAPABILITY_PROPOSE: &str = "external.coding_capability_propose";

    /// Coding Harness operations registered from its manifest.
    pub const CODING_OPERATIONS: &[&str] = &[
        EXECUTION_LIST,
        EXECUTION_READ,
        EXECUTION_WRITE,
        EXECUTION_EXEC,
        EXECUTION_TASK,
        TASK_STATUS,
        CAPABILITY_PROPOSE,
    ];

    /// The active development surface granted to the authorized owner in a
    /// private chat. Proposal construction is receipt-driven through
    /// TASK_SUBMIT; the historical direct proposal tool is not granted.
    /// EXECUTION_TASK is the high-level path-scoped Coding Agent operation
    /// (opencode backend), distinct from the DevelopmentRequest pipeline.
    pub const OWNER_DEVELOPMENT_OPERATIONS: &[&str] = &[
        EXECUTION_LIST,
        EXECUTION_READ,
        EXECUTION_WRITE,
        EXECUTION_EXEC,
        EXECUTION_TASK,
        TASK_SUBMIT,
        TASK_STATUS,
    ];
}

/// Return the registry-level risk for a known coding-harness operation, or
/// `None` for unknown / non-coding operations.
///
/// # Security (P0-A1)
///
/// Unknown operations **must not** receive a default `ReadOnly` risk —
/// that would allow inline execution without gateway approval. Returning
/// `None` forces callers to default to a safe fallback (e.g. `Write`),
/// which requires explicit approval.
pub fn coding_operation_risk(name: &str) -> Option<crate::registry::snapshot::Risk> {
    use crate::registry::snapshot::Risk as SnapshotRisk;
    match name {
        external::EXECUTION_LIST | external::EXECUTION_READ | external::TASK_STATUS => {
            Some(SnapshotRisk::ReadOnly)
        }
        external::EXECUTION_WRITE
        | external::EXECUTION_EXEC
        | external::EXECUTION_TASK
        | external::TASK_SUBMIT
        | external::CAPABILITY_PROPOSE => Some(SnapshotRisk::Write),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::snapshot::Risk;

    #[test]
    fn known_read_operations_map_to_readonly() {
        assert_eq!(
            coding_operation_risk(external::EXECUTION_LIST),
            Some(Risk::ReadOnly)
        );
        assert_eq!(
            coding_operation_risk(external::EXECUTION_READ),
            Some(Risk::ReadOnly)
        );
        assert_eq!(
            coding_operation_risk(external::TASK_STATUS),
            Some(Risk::ReadOnly)
        );
    }

    #[test]
    fn known_write_operations_map_to_write() {
        assert_eq!(
            coding_operation_risk(external::EXECUTION_WRITE),
            Some(Risk::Write)
        );
        assert_eq!(
            coding_operation_risk(external::EXECUTION_EXEC),
            Some(Risk::Write)
        );
        assert_eq!(
            coding_operation_risk(external::EXECUTION_TASK),
            Some(Risk::Write)
        );
        assert_eq!(
            coding_operation_risk(external::TASK_SUBMIT),
            Some(Risk::Write)
        );
        assert_eq!(
            coding_operation_risk(external::CAPABILITY_PROPOSE),
            Some(Risk::Write)
        );
    }

    /// P0-A1: unknown external operations must NOT default to ReadOnly.
    #[test]
    fn unknown_external_operation_is_not_readonly() {
        assert!(coding_operation_risk("external.deploy_anything").is_none());
        assert!(coding_operation_risk("external.write_file_via_new_name").is_none());
        assert!(coding_operation_risk("external.hotload_probe").is_none());
        // Completely unknown names also return None.
        assert!(coding_operation_risk("does.not.exist").is_none());
    }

    /// The model tool view (`provider_tools_for_grants`) must include
    /// EXECUTION_TASK exactly when the principal holds the grant, and must
    /// exclude it (and the whole tool) for principals without it.
    #[test]
    fn provider_tools_view_reflects_execution_task_grant() {
        use crate::domain::operation::external;
        use crate::registry::snapshot::{BindingKind, OperationSpec, RegistrySnapshot};
        use serde_json::json;

        let task_spec = OperationSpec {
            name: external::EXECUTION_TASK.into(),
            risk: Risk::Write,
            description: "path-based coding task".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::External,
            binding_key: format!("binding.{}", external::EXECUTION_TASK),
        };
        let snapshot = RegistrySnapshot {
            snapshot_id: "snap_view".into(),
            created_at: chrono::Utc::now(),
            operations: vec![task_spec],
            hook_bindings: vec![],
        };

        // Owner-style principal WITH the grant sees the tool.
        let granted = vec![external::EXECUTION_TASK.to_string()];
        let tools = snapshot.provider_tools_for_grants(&granted);
        assert_eq!(tools.len(), 1, "granted principal must see the tool");
        assert_eq!(
            tools[0].pointer("/function/name").and_then(|v| v.as_str()),
            Some(external::EXECUTION_TASK),
            "tool view must name external.execution_task"
        );

        // A principal WITHOUT the grant does not see it.
        let ungranted: Vec<String> = vec![];
        assert!(
            snapshot.provider_tools_for_grants(&ungranted).is_empty(),
            "ungranted principal must not see the tool"
        );
    }
}
