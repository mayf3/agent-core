use serde::{Deserialize, Serialize};

/// The action to perform on a harness: enable or disable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessChangeAction {
    Enable,
    Disable,
}

/// An intent to change the active registry snapshot by enabling or disabling
/// an external harness. This is a narrow, authenticated change — not a
/// general-purpose control surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessChangeIntent {
    pub action: HarnessChangeAction,
    pub manifest_id: String,
    pub expected_snapshot_id: String,
    pub requested_by: String,
}

/// A Gateway-approved harness change. The `decision_id` uniquely identifies
/// this approval and is recorded in the `RegistrySnapshotActivated` journal
/// event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedHarnessChange {
    pub intent: HarnessChangeIntent,
    pub decision_id: String,
}

/// Result of a successful activation (enable or disable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryActivationResult {
    pub previous_snapshot_id: String,
    pub active_snapshot_id: String,
    /// True if a new snapshot was created (first enable/disable of a manifest).
    /// False for idempotent re-application.
    pub changed: bool,
}
