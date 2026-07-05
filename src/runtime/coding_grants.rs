//! Feishu coding owner detection and external grant augmentation.
//!
//! Extracted from `runtime/mod.rs` to keep module sizes under the 500-line
//! structure limit. These helpers are called by `Runtime::create_run` and are
//! not used outside the runtime module.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::registry::snapshot::RegistrySnapshot;

/// Check whether the run principal is the configured Feishu coding owner
/// in a private-chat context (source=Feishu, subject matches the configured
/// open_id, chat_type is "p2p"). Only this combination receives the seven
/// `external.coding_*` capability grants.
pub(crate) fn is_coding_owner(
    config: &KernelConfig,
    principal: &RunPrincipal,
    chat_type: Option<&str>,
) -> bool {
    let Some(ref owner_id) = config.feishu_coding_owner_id else {
        return false;
    };
    if principal.source != PrincipalSource::Feishu {
        return false;
    }
    // Group chat: deny even if the sender is the configured owner.
    if chat_type != Some("p2p") {
        return false;
    }
    matches!(&principal.subject, PrincipalSubject::FeishuOpenId(id) if id == owner_id)
}

/// Add external (harness) grants from the pinned snapshot to the principal.
/// Only the configured Feishu coding owner in a private chat receives
/// the exact seven `external.coding_*` grants. Other principals get no
/// coding harness access. Non-coding external operations (e.g. hotload_probe)
/// are always granted for backward compatibility.
pub(crate) fn augment_grants(
    principal: &mut RunPrincipal,
    snapshot: &RegistrySnapshot,
    is_owner: bool,
) {
    for op in &snapshot.operations {
        if op.binding_kind != crate::registry::snapshot::BindingKind::External {
            continue;
        }
        // Non-coding external operations (e.g. hotload_probe) are
        // always granted for backward compatibility.
        let is_coding_op =
            crate::domain::operation::external::CODING_OPERATIONS.contains(&op.name.as_str());
        if is_coding_op && !is_owner {
            continue;
        }
        if !principal.grants.iter().any(|g| g.operation == op.name) {
            principal.grants.push(CapabilityGrant {
                operation: op.name.clone(),
                scope: "current_session".to_string(),
            });
        }
    }
}
