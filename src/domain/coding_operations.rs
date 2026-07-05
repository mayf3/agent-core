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

/// The effective risk for a coding-harness operation based on its side effects.
/// Read operations (list, read, status) are ReadOnly; write operations (write,
/// exec, task_submit, propose) are Write. Non-coding operations default to
/// ReadOnly for backward compatibility with other external harnesses.
/// Returns the registry-level Risk type used in Snapshot operations.
pub fn coding_operation_risk(name: &str) -> crate::registry::snapshot::Risk {
    use crate::registry::snapshot::Risk as SnapshotRisk;
    match name {
        external::WORKSPACE_LIST | external::WORKSPACE_READ | external::TASK_STATUS => {
            SnapshotRisk::ReadOnly
        }
        external::WORKSPACE_WRITE
        | external::WORKSPACE_EXEC
        | external::TASK_SUBMIT
        | external::CAPABILITY_PROPOSE => SnapshotRisk::Write,
        _ => SnapshotRisk::ReadOnly,
    }
}
