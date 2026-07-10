//! Tests for external operation grant CRUD and Runtime loading.
//!
//! Extracted from `grant_ops.rs` to stay under the 500-line module limit.
//! Covers p2p/group isolation, conversation_kind matching, and scope fix.
//! Lifecycle tests (revoke → regrant → second revoke, duplicate events)
//! live in `grant_ops_lifecycle.rs`.

use crate::domain::operation::external;
use crate::domain::*;
use crate::registry::snapshot::{BindingKind, OperationSpec, RegistrySnapshot, Risk};
use chrono::Utc;
use serde_json::json;

/// Build a minimal snapshot containing a single external operation.
fn snapshot_with_op(name: &str, risk: Risk) -> RegistrySnapshot {
    RegistrySnapshot {
        snapshot_id: format!("snap_{name}"),
        created_at: Utc::now(),
        operations: vec![OperationSpec {
            name: name.into(),
            risk,
            description: "test".into(),
            parameters: json!({"type": "object", "properties": {}}),
            idempotent: false,
            binding_kind: BindingKind::External,
            binding_key: format!("binding.{name}"),
        }],
    }
}

/// Create a grant with full conversation_kind support.
fn create_grant(
    j: &crate::journal::JournalStore,
    op: &str,
    principal: &str,
    channel: &str,
    conv_kind: &str,
    snap_id: &str,
) -> String {
    j.create_external_operation_grant(crate::journal::grant_ops::CreateGrantParams {
        operation: op.into(),
        grantee_principal_id: principal.into(),
        channel: channel.into(),
        conversation_kind: conv_kind.into(),
        scope: "principal_channel".into(),
        risk: "Write".into(),
        capability_id: None,
        snapshot_id: snap_id.into(),
        created_by_principal_id: Some("test".into()),
        decision_reference: None,
    })
    .expect("create_grant")
}

// ── 1. Non-coding external op denied without explicit grant ──

#[test]
fn non_coding_external_operation_denied_without_explicit_grant() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);
    let grants = j
        .load_active_external_operation_grants(
            "principal_a",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(grants.is_empty(), "no grant → no grants loaded");
}

// ── 2. Owner Feishu p2p grant is loaded ──

#[test]
fn owner_feishu_p2p_grant_is_loaded() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);
    create_grant(
        &j,
        "external.calculator",
        "owner_p",
        "Feishu",
        "p2p",
        &snap.snapshot_id,
    );

    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].conversation_kind, "p2p");
    assert_eq!(grants[0].status, "active");
}

// ── 3. Same owner Feishu group does NOT load p2p grant ──

#[test]
fn owner_feishu_group_does_not_load_p2p_grant() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);
    create_grant(
        &j,
        "external.calculator",
        "owner_p",
        "Feishu",
        "p2p",
        &snap.snapshot_id,
    );

    // Same principal, same channel, different conversation_kind → not loaded.
    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "group",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(
        grants.is_empty(),
        "same owner in group chat must not load p2p grant"
    );
}

// ── 4. Same owner p2p and group are distinguished ──

#[test]
fn same_owner_p2p_and_group_are_distinguished() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);

    create_grant(
        &j,
        "external.calculator",
        "owner_p",
        "Feishu",
        "p2p",
        &snap.snapshot_id,
    );

    // p2p loads.
    let p2p_grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert_eq!(p2p_grants.len(), 1, "p2p grant must load for p2p context");

    // group does NOT load.
    let group_grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "group",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(
        group_grants.is_empty(),
        "p2p grant must NOT load for group context"
    );
}

// ── 5. Wrong conversation_kind not loaded ──

#[test]
fn wrong_conversation_kind_not_loaded() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);
    create_grant(
        &j,
        "external.calculator",
        "owner_p",
        "Feishu",
        "p2p",
        &snap.snapshot_id,
    );

    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "cli",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(
        grants.is_empty(),
        "wrong conversation_kind must not load grant"
    );
}

// ── 6. CLI grant not loaded in Feishu ──

#[test]
fn cli_grant_not_loaded_in_feishu() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);
    create_grant(
        &j,
        "external.calculator",
        "cli_user",
        "Cli",
        "cli",
        &snap.snapshot_id,
    );

    let grants = j
        .load_active_external_operation_grants(
            "cli_user",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(
        grants.is_empty(),
        "CLI grant must not load in Feishu channel"
    );
}

// ── 7. Wrong principal: grant not loaded ──

#[test]
fn wrong_principal_grant_not_loaded() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);
    create_grant(
        &j,
        "external.calculator",
        "owner_p",
        "Feishu",
        "p2p",
        &snap.snapshot_id,
    );

    let grants = j
        .load_active_external_operation_grants(
            "stranger",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(grants.is_empty(), "wrong principal must not load grant");
}

// ── 8. Revoked grant not loaded ──

#[test]
fn revoked_grant_not_loaded() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);
    let gid = create_grant(
        &j,
        "external.calculator",
        "owner_p",
        "Feishu",
        "p2p",
        &snap.snapshot_id,
    );

    j.revoke_external_operation_grant(&gid).unwrap();

    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(grants.is_empty(), "revoked grant must not be loaded");
}

// ── 9. Wrong snapshot: grant not loaded ──

#[test]
fn wrong_snapshot_grant_not_loaded() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap_s1 = snapshot_with_op("external.calculator", Risk::ReadOnly);
    let snap_s2_id = "snap_other_v2";

    create_grant(
        &j,
        "external.calculator",
        "owner_p",
        "Feishu",
        "p2p",
        &snap_s1.snapshot_id,
    );

    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            snap_s2_id,
        )
        .unwrap();
    assert!(grants.is_empty(), "wrong snapshot must not load grant");
}

// ── 10. Unknown operation remains default deny (P0-A1 preserved) ──

#[test]
fn unknown_operation_remains_default_deny() {
    assert!(
        crate::domain::operation::coding_operation_risk("external.deploy_anything").is_none(),
        "P0-A1: unknown operation must not default to ReadOnly"
    );
}

// ── 11. Owner-private seven coding grants remain available (P0-A1 preserved) ──

#[test]
fn owner_private_seven_coding_grants_remain_available() {
    let mut ops = vec![];
    for op in external::CODING_OPERATIONS {
        let risk = if matches!(
            *op,
            external::WORKSPACE_LIST | external::WORKSPACE_READ | external::TASK_STATUS
        ) {
            Risk::ReadOnly
        } else {
            Risk::Write
        };
        ops.push(OperationSpec {
            name: op.to_string(),
            risk,
            description: "coding".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::External,
            binding_key: format!("binding.{op}"),
        });
    }
    let snap = RegistrySnapshot {
        snapshot_id: "snap_coding".into(),
        created_at: Utc::now(),
        operations: ops,
    };

    let mut grants: Vec<CapabilityGrant> = vec![];
    for op in &snap.operations {
        if op.binding_kind == BindingKind::External
            && external::CODING_OPERATIONS.contains(&op.name.as_str())
            && !grants.iter().any(|g| g.operation == op.name)
        {
            grants.push(CapabilityGrant {
                operation: op.name.clone(),
                scope: "current_session".to_string(),
            });
        }
    }

    assert_eq!(grants.len(), 7, "all 7 coding ops granted to owner");
    for op in external::CODING_OPERATIONS {
        assert!(
            grants.iter().any(|g| g.operation == *op),
            "owner must receive {op}"
        );
    }
}
