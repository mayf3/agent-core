//! Lifecycle and idempotency tests for external operation grants.
//!
//! Extracted from `grant_ops.rs` to stay under the 500-line module limit.
//! These tests exercise create → revoke → regrant → revoke-again and
//! journal event correctness with conversation_kind isolation.

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

/// Create a grant with p2p conversation kind.
fn create_p2p_grant(
    j: &crate::journal::JournalStore,
    op: &str,
    principal: &str,
    snap_id: &str,
) -> String {
    j.create_external_operation_grant(crate::journal::grant_ops::CreateGrantParams {
        operation: op.into(),
        grantee_principal_id: principal.into(),
        channel: "Feishu".into(),
        conversation_kind: "p2p".into(),
        scope: "principal_channel".into(),
        risk: "Write".into(),
        capability_id: None,
        snapshot_id: snap_id.into(),
        created_by_principal_id: Some("test".into()),
        decision_reference: None,
    })
    .expect("create_p2p_grant")
}

// ── Critical lifecycle: grant → revoke → regrant → second revoke ──

#[test]
fn grant_can_be_revoked_regranted_and_revoked_again() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);

    // 1. Create grant A (Feishu p2p)
    let gid_a = create_p2p_grant(&j, "external.calculator", "owner_p", &snap.snapshot_id);
    assert!(gid_a.starts_with("grt_"), "grant_id must start with grt_");

    // Verify 1 active grant
    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert_eq!(grants.len(), 1, "exactly 1 active grant after first create");

    // 2. Revoke grant A
    j.revoke_external_operation_grant(&gid_a).unwrap();
    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(grants.is_empty(), "0 active grants after revoke");

    // 3. Re-create same logical grant B (partial unique index excludes revoked rows)
    let gid_b = create_p2p_grant(&j, "external.calculator", "owner_p", &snap.snapshot_id);
    assert_ne!(
        gid_a, gid_b,
        "re-grant must produce DIFFERENT grant_id (new row)"
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
    assert_eq!(grants.len(), 1, "exactly 1 active grant after regrant");
    assert_eq!(grants[0].grant_id, gid_b, "active grant must be gid_b");

    // 4. Revoke grant B
    j.revoke_external_operation_grant(&gid_b).unwrap();
    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(grants.is_empty(), "0 active grants after second revoke");

    // 5. Audit: BOTH history rows exist (no rows deleted)
    let conn = j.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM external_operation_grants
             WHERE operation = 'external.calculator' AND grantee_principal_id = 'owner_p'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2, "both history rows must exist for audit trail");
    drop(conn);
}

// ── Duplicate create returns real persisted grant_id ──

#[test]
fn duplicate_create_returns_existing_persisted_grant_id() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);

    let gid1 = create_p2p_grant(&j, "external.calculator", "owner_p", &snap.snapshot_id);
    let gid2 = create_p2p_grant(&j, "external.calculator", "owner_p", &snap.snapshot_id);

    // With the fix, duplicate create returns THE SAME persistent grant_id.
    assert_eq!(gid1, gid2, "duplicate create must return existing grant_id");

    // Only one row in DB.
    let conn = j.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM external_operation_grants
             WHERE operation = 'external.calculator' AND grantee_principal_id = 'owner_p'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "exactly one row in DB");
    drop(conn);

    // gid1 can still be revoked (it's the real grant_id).
    j.revoke_external_operation_grant(&gid1).unwrap();
    let grants = j
        .load_active_external_operation_grants(
            "owner_p",
            "Feishu",
            "p2p",
            "principal_channel",
            &snap.snapshot_id,
        )
        .unwrap();
    assert!(grants.is_empty(), "revoke must succeed");
}

// ── Duplicate create must NOT emit duplicate grant event ──

#[test]
fn duplicate_create_emits_single_granted_event() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);

    let _gid1 = create_p2p_grant(&j, "external.calculator", "owner_p", &snap.snapshot_id);
    let _gid2 = create_p2p_grant(&j, "external.calculator", "owner_p", &snap.snapshot_id);

    let events = j.events().unwrap();
    let granted_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::ExternalOperationGranted)
        .collect();

    assert_eq!(
        granted_events.len(),
        1,
        "duplicate create must not emit a second ExternalOperationGranted event"
    );
}

// ── Revoke emits event only on active → revoked transition ──

#[test]
fn revoke_emits_event_only_on_active_to_revoked_transition() {
    let j = crate::journal::JournalStore::in_memory().unwrap();
    let snap = snapshot_with_op("external.calculator", Risk::ReadOnly);

    let gid = create_p2p_grant(&j, "external.calculator", "owner_p", &snap.snapshot_id);

    // First revoke: active → revoked, should emit event
    j.revoke_external_operation_grant(&gid).unwrap();

    let events = j.events().unwrap();
    let revoked_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::ExternalOperationRevoked)
        .collect();
    assert_eq!(
        revoked_events.len(),
        1,
        "first revoke must emit exactly one ExternalOperationRevoked event"
    );

    // Second revoke: already revoked → no-op, no event
    j.revoke_external_operation_grant(&gid).unwrap();

    let events2 = j.events().unwrap();
    let revoked_events2: Vec<_> = events2
        .iter()
        .filter(|e| e.kind == JournalEventKind::ExternalOperationRevoked)
        .collect();
    assert_eq!(
        revoked_events2.len(),
        1,
        "second revoke must NOT emit another event"
    );
}
