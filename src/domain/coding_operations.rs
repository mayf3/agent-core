//! Coding-harness operation names and risk classification.
//!
//! Extracted from `operation.rs` to keep module sizes under the 500-line
//! structure limit. The `external` submodule and `coding_operation_risk`
//! are re-exported from `operation.rs` so all existing import paths work.

/// Known external (harness-registered) operation name constants.
pub mod external {
    pub const WORKSPACE_LIST: &str = "external.coding_workspace_list";
    pub const WORKSPACE_READ: &str = "external.coding_workspace_read";
    pub const WORKSPACE_WRITE: &str = "external.coding_workspace_write";
    pub const WORKSPACE_EXEC: &str = "external.coding_workspace_exec";
    pub const TASK_SUBMIT: &str = "external.coding_task_submit";
    pub const TASK_STATUS: &str = "external.coding_task_status";
    pub const HCR_ACCEPT: &str = "external.coding_hcr_accept";
    pub const CAPABILITY_PROPOSE: &str = "external.coding_capability_propose";

    /// The exact set of seven coding-harness operations that an authorized
    /// owner receives in a private chat. Every other access path is denied.
    pub const CODING_OPERATIONS: &[&str] = &[
        WORKSPACE_LIST,
        WORKSPACE_READ,
        WORKSPACE_WRITE,
        WORKSPACE_EXEC,
        TASK_SUBMIT,
        TASK_STATUS,
        CAPABILITY_PROPOSE,
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
        external::WORKSPACE_LIST | external::WORKSPACE_READ | external::TASK_STATUS => {
            Some(SnapshotRisk::ReadOnly)
        }
        external::WORKSPACE_WRITE
        | external::WORKSPACE_EXEC
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
            coding_operation_risk(external::WORKSPACE_LIST),
            Some(Risk::ReadOnly)
        );
        assert_eq!(
            coding_operation_risk(external::WORKSPACE_READ),
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
            coding_operation_risk(external::WORKSPACE_WRITE),
            Some(Risk::Write)
        );
        assert_eq!(
            coding_operation_risk(external::WORKSPACE_EXEC),
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
}
