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
/// the exact seven `external.coding_*` grants. Non-coding external operations
/// are never auto-granted — they require explicit grant configuration.
/// Unknown external operations are never auto-granted either.
pub(crate) fn augment_grants(
    principal: &mut RunPrincipal,
    snapshot: &RegistrySnapshot,
    is_owner: bool,
) {
    // Only the coding owner receives external grants.
    if !is_owner {
        return;
    }
    for op in &snapshot.operations {
        if op.binding_kind != crate::registry::snapshot::BindingKind::External {
            continue;
        }
        // Only grant known coding operations — non-coding external ops
        // (e.g. hotload_probe, deploy_anything) are NOT auto-granted.
        if !crate::domain::operation::external::CODING_OPERATIONS.contains(&op.name.as_str()) {
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

/// The subset of coding operations allowed in HCR mode.
///
/// Only workspace operations required for Route A harness creation are permitted.
/// Task submission, capability proposals, and other operations are excluded
/// from HCR mode and will be denied by the policy pipeline.
pub fn hcr_allowed_operations() -> &'static [&'static str] {
    &[
        crate::domain::operation::external::WORKSPACE_LIST,
        crate::domain::operation::external::WORKSPACE_READ,
        crate::domain::operation::external::WORKSPACE_WRITE,
        crate::domain::operation::external::WORKSPACE_EXEC,
    ]
}

/// Check whether an operation is allowed in HCR mode.
pub fn is_hcr_allowed_operation(operation: &str) -> bool {
    hcr_allowed_operations().contains(&operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::operation::external;
    use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
    use serde_json::json;

    fn owner_principal() -> RunPrincipal {
        RunPrincipal {
            principal_id: PrincipalId("owner".into()),
            subject: PrincipalSubject::FeishuOpenId("o_owner".into()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: None,
        }
    }

    fn non_owner_principal() -> RunPrincipal {
        RunPrincipal {
            principal_id: PrincipalId("non_owner".into()),
            subject: PrincipalSubject::FeishuOpenId("o_stranger".into()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: None,
        }
    }

    fn external_spec(name: &str, risk: Risk) -> OperationSpec {
        OperationSpec {
            name: name.into(),
            risk,
            description: "test".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::External,
            binding_key: format!("binding.{name}"),
        }
    }

    /// P0-A1: unknown external operations are NOT auto-granted to any principal.
    #[test]
    fn unknown_external_operation_is_not_auto_granted() {
        let snapshot = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: chrono::Utc::now(),
            operations: vec![
                external_spec("external.deploy_anything", Risk::Write),
                external_spec("external.write_file_via_new_name", Risk::Write),
            ],
        };

        // Non-owner: no grants.
        let mut principal = non_owner_principal();
        augment_grants(&mut principal, &snapshot, false);
        assert!(principal.grants.is_empty(), "non-owner gets no grants");

        // Owner: still no grants (unknown non-coding ops are not auto-granted).
        let mut principal = owner_principal();
        augment_grants(&mut principal, &snapshot, true);
        assert!(
            principal.grants.is_empty(),
            "owner does not receive non-coding external grants"
        );
    }

    /// P0-A1: non-coding external operations are NOT auto-granted by default,
    /// even for the owner.
    #[test]
    fn non_coding_external_operation_is_not_auto_granted_by_default() {
        let snapshot = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: chrono::Utc::now(),
            operations: vec![
                external_spec("external.hotload_probe", Risk::Write),
                external_spec("external.time_now", Risk::ReadOnly),
            ],
        };

        let mut principal = owner_principal();
        augment_grants(&mut principal, &snapshot, true);
        assert!(
            principal.grants.is_empty(),
            "owner does not receive non-coding external grants: {:?}",
            principal.grants
        );
    }

    /// P0-A1 preserved: owner in private chat receives coding operation grants.
    #[test]
    fn owner_private_coding_grants_still_work() {
        let snapshot = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: chrono::Utc::now(),
            operations: vec![
                external_spec(external::WORKSPACE_LIST, Risk::ReadOnly),
                external_spec(external::WORKSPACE_READ, Risk::ReadOnly),
                external_spec(external::WORKSPACE_WRITE, Risk::Write),
                external_spec(external::WORKSPACE_EXEC, Risk::Write),
                external_spec(external::TASK_SUBMIT, Risk::Write),
                external_spec(external::TASK_STATUS, Risk::ReadOnly),
                external_spec(external::CAPABILITY_PROPOSE, Risk::Write),
            ],
        };

        let mut principal = owner_principal();
        augment_grants(&mut principal, &snapshot, true);

        let granted_ops: Vec<&str> = principal
            .grants
            .iter()
            .map(|g| g.operation.as_str())
            .collect();
        assert_eq!(granted_ops.len(), 7, "all 7 coding ops granted");
        for op in external::CODING_OPERATIONS {
            assert!(granted_ops.contains(op), "{op} not granted");
        }
    }

    /// P0-A1: non-owner / group chat does NOT receive coding operation grants.
    #[test]
    fn group_chat_or_non_owner_does_not_receive_coding_write_exec_grants() {
        let snapshot = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: chrono::Utc::now(),
            operations: vec![
                external_spec(external::WORKSPACE_WRITE, Risk::Write),
                external_spec(external::WORKSPACE_EXEC, Risk::Write),
                external_spec(external::TASK_SUBMIT, Risk::Write),
                external_spec(external::CAPABILITY_PROPOSE, Risk::Write),
            ],
        };

        let mut principal = non_owner_principal();
        augment_grants(&mut principal, &snapshot, false);
        assert!(
            principal.grants.is_empty(),
            "non-owner must not receive coding grants: {:?}",
            principal.grants
        );
    }
}
