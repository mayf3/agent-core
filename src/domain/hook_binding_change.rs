//! Narrow, authenticated HookBinding change intent for the Trusted Control
//! Plane (online bind / unbind of a hook contract binding).
//!
//! Mirrors the harness change pattern: an intent is approved by the Gateway
//! (ipc_operator only) and executed by the Journal through the existing
//! snapshot activation flow. No new permission system, no approval UI, no
//! Feishu approval.

use crate::registry::snapshot::HookBinding;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookBindingChangeAction {
    /// Set (or replace) the binding for the target contract.
    Bind,
    /// Remove the binding for the target contract.
    Unbind,
}

/// An intent to change the active registry snapshot's hook binding for one
/// contract. This is a narrow, authenticated change — not a general-purpose
/// control surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookBindingChangeIntent {
    pub action: HookBindingChangeAction,
    /// Target hook contract (e.g. `context.prepare.v0`).
    pub contract: String,
    /// For `Bind`: the full binding to set (existing binding for the same
    /// contract is replaced). For `Unbind`: `None`.
    pub binding: Option<HookBinding>,
    pub expected_snapshot_id: String,
    pub requested_by: String,
}

/// A Gateway-approved hook binding change. The `decision_id` uniquely
/// identifies this approval and is recorded in the `RegistrySnapshotActivated`
/// journal event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedHookBindingChange {
    pub intent: HookBindingChangeIntent,
    pub decision_id: String,
}
