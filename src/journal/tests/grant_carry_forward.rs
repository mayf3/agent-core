//! Tests for `carry_forward_external_operation_grants`.
//!
//! Verifies that active grants on a stale snapshot are carried forward
//! to the current snapshot ONLY when the operation binding is identical
//! (same binding_kind AND binding_key). Old grant rows are never modified.

use crate::domain::*;
use crate::journal::grant_ops::CreateGrantParams;
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

/// Create a grant on the given store for a given operation and snapshot.
fn create_grant(
    j: &crate::journal::JournalStore,
    op: &str,
    snap_id: &str,
) -> String {
    j.create_external_operation_grant(CreateGrantParams {
        operation: op.into(),
        grantee_principal_id: "user:test".into(),
        channel: "Feishu".into(),
        conversation_kind: "p2p".into(),
        scope: "principal_channel".into(),
        risk: "ReadOnly".into(),
        capability_id: None,
        snapshot_id: snap_id.into(),
        created_by_principal_id: Some("test".into()),
        decision_reference: None,
    })
    .expect("create_grant")
}

/// Count active grants for a given snapshot.
fn count_grants(j: &crate::journal::JournalStore, snap_id: &str) -> usize {
    j.load_active_external_operation_grants("user:test", "Feishu", "p2p", "principal_channel", snap_id)
        .expect("load_grants")
        .len()
}

/// Count ALL active grants (any snapshot).
fn count_all_active(j: &crate::journal::JournalStore) -> i64 {
    let conn = j.conn.lock().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM external_operation_grants WHERE status = 'active'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// Persist and activate a new snapshot, returning its ID.
fn activate_new_snapshot(
    j: &crate::journal::JournalStore,
    ops: Vec<OperationSpec>,
) -> String {
    let snap = j.create_registry_snapshot(ops).expect("create snapshot");
    let new_id = snap.snapshot_id.clone();
    let old_id = j.get_current_snapshot_id_for_test().unwrap();
    let decision_id = format!("test:{}->{}", &old_id[..8], &new_id[..8]);
    j.activate_snapshot_transactional(&old_id, &new_id, &decision_id, "test_activation")
        .expect("activate snapshot");
    // Verify the cache was updated by activation
    let cached = j.get_current_snapshot_id_for_test().unwrap();
    assert_eq!(cached, new_id, "activate must update current_snapshot_id cache");
    new_id
}

/// Build an OperationSpec for test use.
fn op_spec(
    name: &str,
    binding_kind: BindingKind,
    binding_key: &str,
) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        risk: Risk::ReadOnly,
        description: "test".into(),
        parameters: json!({"type": "object"}),
        idempotent: false,
        binding_kind,
        binding_key: binding_key.into(),
    }
}

// ── 1. Old grant rows are unchanged ──

#[test]
fn old_grant_row_snapshot_id_is_preserved() {
    let j = crate::journal::JournalStore::in_memory().unwrap();

    // Create S1 with two operations: 'external.foo' and 's1_only'.
    // The 's1_only' op ensures S1 and S2 have different content-addressed IDs.
    let s1_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
        op_spec("s1_only", BindingKind::External, "manifest.v1"),
    ]);
    create_grant(&j, "external.foo", &s1_id);
    create_grant(&j, "s1_only", &s1_id);

    // Create S2 with 'external.foo' (identical) plus 's2_only' (different).
    let s2_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
        op_spec("s2_only", BindingKind::External, "manifest.v1"),
    ]);

    // Carry forward — only 'external.foo' is identical; 's1_only' is gone.
    let carried = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried, 1, "one grant should be carried forward");

    // Old grant row still references S1 — both grants still on S1.
    let old_grant_count = count_grants(&j, &s1_id);
    assert_eq!(old_grant_count, 2, "both old grant rows on S1 must be preserved");

    // New grant row exists on S2 (only for 'external.foo').
    let new_grant_count = count_grants(&j, &s2_id);
    assert_eq!(new_grant_count, 1, "carried-forward grant must exist on S2");

    // Total active grants = 2 (S1) + 1 (S2) = 3 (old unchanged + new copy).
    assert_eq!(count_all_active(&j), 3, "total active grants must be 3 (2 old + 1 new)");
}

// ── 2. Binding key change → not carried forward ──

#[test]
fn changed_binding_key_not_carried_forward() {
    let j = crate::journal::JournalStore::in_memory().unwrap();

    let s1_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
    ]);
    create_grant(&j, "external.foo", &s1_id);

    // S2: same operation, same binding_kind, DIFFERENT binding_key.
    let s2_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v2"),
    ]);

    let carried = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried, 0, "binding key change must NOT carry forward");

    // Only the old grant should exist.
    assert_eq!(count_all_active(&j), 1, "no new grant should be created");
}

// ── 3. Binding kind change → not carried forward ──

#[test]
fn changed_binding_kind_not_carried_forward() {
    let j = crate::journal::JournalStore::in_memory().unwrap();

    let s1_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
    ]);
    create_grant(&j, "external.foo", &s1_id);

    // S2: same operation, DIFFERENT binding_kind (Builtin vs External).
    let s2_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::Builtin, "manifest.v1"),
    ]);

    let carried = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried, 0, "binding kind change must NOT carry forward");

    assert_eq!(count_all_active(&j), 1, "no new grant should be created");
}

// ── 4. Operation removed → not carried forward ──

#[test]
fn removed_operation_not_carried_forward() {
    let j = crate::journal::JournalStore::in_memory().unwrap();

    let s1_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
        op_spec("external.bar", BindingKind::External, "manifest.v1"),
    ]);
    create_grant(&j, "external.foo", &s1_id);
    create_grant(&j, "external.bar", &s1_id);

    // S2: removes external.bar.
    let s2_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
    ]);

    let carried = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried, 1, "only 'foo' (present in both) should carry forward");

    // foo has a grant on S2, bar does not.
    assert_eq!(count_grants(&j, &s2_id), 1, "only 'foo' grant on S2");
    let s2_grants = j.load_active_external_operation_grants(
        "user:test", "Feishu", "p2p", "principal_channel", &s2_id,
    ).unwrap();
    assert_eq!(s2_grants[0].operation, "external.foo");

    // Old grants on S1 still intact.
    assert_eq!(count_grants(&j, &s1_id), 2, "all old grants preserved on S1");
}

// ── 5. Idempotent — repeated calls do not duplicate ──

#[test]
fn repeated_carry_forward_is_idempotent() {
    let j = crate::journal::JournalStore::in_memory().unwrap();

    // S1 and S2 need different overall content so their IDs differ.
    let s1_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
        op_spec("s1_only", BindingKind::External, "manifest.v1"),
    ]);
    create_grant(&j, "external.foo", &s1_id);

    let s2_id = activate_new_snapshot(&j, vec![
        op_spec("external.foo", BindingKind::External, "manifest.v1"),
        op_spec("s2_only", BindingKind::External, "manifest.v1"),
    ]);

    // First call.
    let carried1 = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried1, 1, "first call carries forward");

    // Second call — should be a no-op.
    let carried2 = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried2, 0, "second call must be idempotent (0 carried)");

    // Total active grants unchanged by second call.
    assert_eq!(count_all_active(&j), 2, "total grants must not grow on repeat");

    // Exactly one grant on S2.
    assert_eq!(count_grants(&j, &s2_id), 1, "single grant on S2 after repeat");
}

// ── 6. Mixed scenario: unchanged + changed + removed ──

#[test]
fn mixed_scenario_selective_carry_forward() {
    let j = crate::journal::JournalStore::in_memory().unwrap();

    let s1_id = activate_new_snapshot(&j, vec![
        op_spec("stable.op", BindingKind::External, "manifest.v1"),
        op_spec("changed_key.op", BindingKind::External, "manifest.v1"),
        op_spec("removed.op", BindingKind::External, "manifest.v1"),
    ]);
    // Grant all three.
    create_grant(&j, "stable.op", &s1_id);
    create_grant(&j, "changed_key.op", &s1_id);
    create_grant(&j, "removed.op", &s1_id);

    // S2: 'stable.op' unchanged; 'changed_key.op' has new binding key; 'removed.op' gone.
    let s2_id = activate_new_snapshot(&j, vec![
        op_spec("stable.op", BindingKind::External, "manifest.v1"),
        op_spec("changed_key.op", BindingKind::External, "manifest.v2"),
    ]);

    let carried = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried, 1, "only 'stable.op' should carry forward");

    // Only 'stable.op' has a grant on S2.
    let s2_grants = j.load_active_external_operation_grants(
        "user:test", "Feishu", "p2p", "principal_channel", &s2_id,
    ).unwrap();
    assert_eq!(s2_grants.len(), 1);
    assert_eq!(s2_grants[0].operation, "stable.op");

    // All 3 old grants still on S1.
    assert_eq!(count_grants(&j, &s1_id), 3);

    // Total active = 3 (S1) + 1 (S2) = 4.
    assert_eq!(count_all_active(&j), 4);
}

// ── 7. No stale grants → no-op ──

#[test]
fn no_stale_grants_is_noop() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let s1_id = j.get_current_snapshot_id_for_test().unwrap();

    create_grant(&j, "external.foo", &s1_id);

    // Current snapshot already matches grant's snapshot — nothing stale.
    let carried = j.carry_forward_external_operation_grants().unwrap();
    assert_eq!(carried, 0, "no stale grants -> no carry forward");

    assert_eq!(count_all_active(&j), 1, "single grant unchanged");
}
