use super::*;
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, OperationSpec, RegistrySnapshot, Risk};
use serde_json::json;

fn make_cli_snapshot() -> RegistrySnapshot {
    RegistrySnapshot {
        snapshot_id: "snap_test".into(),
        created_at: chrono::Utc::now(),
        operations: vec![
            OperationSpec {
                name: "stdout.send_text".into(),
                risk: Risk::Write,
                description: "".into(),
                parameters: json!({"type": "object"}),
                idempotent: false,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.stdout".into(),
            },
            OperationSpec {
                name: "session.recall_recent".into(),
                risk: Risk::ReadOnly,
                description: "".into(),
                parameters: json!({"type": "object"}),
                idempotent: true,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.recall".into(),
            },
            OperationSpec {
                name: "harness.op".into(),
                risk: Risk::ReadOnly,
                description: "harness".into(),
                parameters: json!({"type": "object"}),
                idempotent: true,
                binding_kind: BindingKind::ExternalHarness,
                binding_key: "harness:hash:op".into(),
            },
        ],
    }
}

fn in_memory_journal() -> JournalStore {
    JournalStore::in_memory().expect("in-memory journal")
}

#[test]
fn grant_then_list_shows_grant() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    let grants = list_grants(&journal, None).unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].channel, "Cli");
    assert_eq!(grants[0].operation_name, "harness.op");
}

#[test]
fn grant_is_idempotent() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    let grants = list_grants(&journal, None).unwrap();
    assert_eq!(grants.len(), 1, "idempotent grant must not duplicate");
}

#[test]
fn revoke_removes_grant() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    revoke_operation(&journal, "Cli", "harness.op").unwrap();
    let grants = list_grants(&journal, None).unwrap();
    assert_eq!(grants.len(), 0);
}

#[test]
fn revoke_is_idempotent() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    revoke_operation(&journal, "Cli", "harness.op").unwrap();
    revoke_operation(&journal, "Cli", "harness.op").unwrap();
    let grants = list_grants(&journal, None).unwrap();
    assert_eq!(grants.len(), 0);
}

#[test]
fn list_grants_by_channel() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "op1").unwrap();
    grant_operation(&journal, "Feishu", "op2").unwrap();
    let cli_grants = list_grants(&journal, Some("Cli")).unwrap();
    assert_eq!(cli_grants.len(), 1);
    assert_eq!(cli_grants[0].operation_name, "op1");
    let feishu_grants = list_grants(&journal, Some("Feishu")).unwrap();
    assert_eq!(feishu_grants.len(), 1);
    assert_eq!(feishu_grants[0].operation_name, "op2");
}

#[test]
fn unknown_channel_rejected() {
    let journal = in_memory_journal();
    assert!(grant_operation(&journal, "Unknown", "op").is_err());
    assert!(revoke_operation(&journal, "Unknown", "op").is_err());
}

#[test]
fn derive_grants_includes_baseline_and_db_grants() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    let snapshot = make_cli_snapshot();
    let grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
    let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
    assert!(names.contains(&"stdout.send_text"));
    assert!(names.contains(&"session.recall_recent"));
    assert!(names.contains(&"harness.op"));
}

#[test]
fn derive_grants_filters_out_operations_not_in_snapshot() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "missing.op").unwrap();
    let snapshot = make_cli_snapshot();
    let grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
    let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
    assert!(!names.contains(&"missing.op"), "must be filtered out");
}

#[test]
fn derive_grants_no_db_grant_still_has_baseline() {
    let journal = in_memory_journal();
    let snapshot = make_cli_snapshot();
    let grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
    let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
    assert!(
        names.contains(&"stdout.send_text"),
        "baseline must be present"
    );
    assert_eq!(names.len(), 2, "only baseline (no extra, no grants)");
}

#[test]
fn derive_grants_respects_extra_allowed_operations() {
    let journal = in_memory_journal();
    let snapshot = make_cli_snapshot();
    let extras = vec!["system.status".to_string()];
    let grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &extras).unwrap();
    let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
    assert!(
        names.contains(&"stdout.send_text"),
        "baseline must be present"
    );
}

#[test]
fn derive_grants_respects_extra_allowed_operations_does_not_break_old_compat() {
    let journal = in_memory_journal();
    let snapshot = make_cli_snapshot();
    let extras = vec!["system.status".to_string()];
    let grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &extras).unwrap();
    let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
    assert!(
        !names.contains(&"system.status"),
        "not in this test snapshot, so should be filtered out"
    );
    assert!(names.contains(&"stdout.send_text"));
}

#[test]
fn feishu_and_cli_grants_are_independent() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "cli_op").unwrap();
    grant_operation(&journal, "Feishu", "feishu_op").unwrap();

    let cli_snapshot = RegistrySnapshot {
        snapshot_id: "snap_cli".into(),
        created_at: chrono::Utc::now(),
        operations: vec![
            OperationSpec {
                name: "stdout.send_text".into(),
                risk: Risk::Write,
                description: "".into(),
                parameters: json!({"type": "object"}),
                idempotent: false,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.stdout".into(),
            },
            OperationSpec {
                name: "session.recall_recent".into(),
                risk: Risk::ReadOnly,
                description: "".into(),
                parameters: json!({"type": "object"}),
                idempotent: true,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.recall".into(),
            },
            OperationSpec {
                name: "cli_op".into(),
                risk: Risk::ReadOnly,
                description: "".into(),
                parameters: json!({"type": "object"}),
                idempotent: true,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.cli_op".into(),
            },
        ],
    };

    let feishu_snapshot = RegistrySnapshot {
        snapshot_id: "snap_feishu".into(),
        created_at: chrono::Utc::now(),
        operations: vec![
            OperationSpec {
                name: "feishu.send_message".into(),
                risk: Risk::Write,
                description: "".into(),
                parameters: json!({"type": "object"}),
                idempotent: false,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.feishu".into(),
            },
            OperationSpec {
                name: "feishu_op".into(),
                risk: Risk::ReadOnly,
                description: "".into(),
                parameters: json!({"type": "object"}),
                idempotent: true,
                binding_kind: BindingKind::ExternalHarness,
                binding_key: "harness:hash:feishu_op".into(),
            },
        ],
    };

    let cli_grants = derive_grants(&journal, &ChannelKind::Cli, &cli_snapshot, &[]).unwrap();
    let feishu_grants =
        derive_grants(&journal, &ChannelKind::Feishu, &feishu_snapshot, &[]).unwrap();

    let cli_names: Vec<&str> = cli_grants.iter().map(|g| g.operation.as_str()).collect();
    let feishu_names: Vec<&str> = feishu_grants.iter().map(|g| g.operation.as_str()).collect();

    assert!(
        cli_names.contains(&"cli_op"),
        "cli should see cli_op: {cli_names:?}"
    );
    assert!(
        !cli_names.contains(&"feishu_op"),
        "cli should NOT see feishu_op: {cli_names:?}"
    );
    assert!(
        feishu_names.contains(&"feishu_op"),
        "feishu should see feishu_op: {feishu_names:?}"
    );
}

#[test]
fn existing_run_grants_unchanged_by_later_grant_or_revoke() {
    let journal = in_memory_journal();
    let snapshot = make_cli_snapshot();

    let run_grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
    assert_eq!(run_grants.len(), 2);

    grant_operation(&journal, "Cli", "harness.op").unwrap();
    let new_run_grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
    assert_eq!(new_run_grants.len(), 3, "new run should see new grant");
    assert_eq!(run_grants.len(), 2, "old run grants unchanged");

    revoke_operation(&journal, "Cli", "harness.op").unwrap();
    let after_revoke_grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
    assert_eq!(
        after_revoke_grants.len(),
        2,
        "new run after revoke loses grant"
    );
}

// --- Transaction atomicity tests ---

#[test]
fn grant_event_written_inside_transaction() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "test.op").unwrap();
    let grants = list_grants(&journal, Some("Cli")).unwrap();
    assert_eq!(grants.len(), 1);
    let conn = journal.conn.lock().unwrap();
    let event_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM journal_events WHERE kind = 'OperationGrantChanged'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 1, "grant event must exist");
}

#[test]
fn grant_rollback_on_event_failure() {
    let journal = in_memory_journal();

    let mut conn = journal.conn.lock().unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();

    let change = grant_operation_in_transaction(&tx, "Cli", "rollback_test", "now").unwrap();
    assert_eq!(
        change,
        GrantChange::Changed(OperationGrant {
            channel: "Cli".into(),
            operation_name: "rollback_test".into(),
            created_at: "now".into(),
        })
    );

    let event_result = crate::journal::hash_chain::append_event_in_transaction(
        &tx,
        "OperationGrantChanged",
        r#"{"channel":"Cli","operation_name":"rollback_test","action":"granted"}"#,
        "now",
    );
    assert!(event_result.is_ok(), "event append must succeed");
    drop(tx);
    drop(conn);

    let grants = list_grants(&journal, Some("Cli")).unwrap();
    assert!(
        !grants.iter().any(|g| g.operation_name == "rollback_test"),
        "grant must not exist after rollback"
    );
}

#[test]
fn revoke_rollback_on_event_failure() {
    let journal = in_memory_journal();
    {
        let conn = journal.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO channel_operation_grants (channel, operation_name, created_at) VALUES ('Cli', 'rollback_test', 'now')",
            [],
        ).unwrap();
    }

    let mut conn = journal.conn.lock().unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();

    let change = revoke_operation_in_transaction(&tx, "Cli", "rollback_test").unwrap();
    assert_eq!(change, RevokeChange::Changed);

    let event_result = crate::journal::hash_chain::append_event_in_transaction(
        &tx,
        "OperationGrantChanged",
        r#"{"channel":"Cli","operation_name":"rollback_test","action":"revoked"}"#,
        "now",
    );
    assert!(event_result.is_ok(), "event append must succeed");
    drop(tx);
    drop(conn);

    let grants = list_grants(&journal, Some("Cli")).unwrap();
    assert!(
        grants.iter().any(|g| g.operation_name == "rollback_test"),
        "grant must still exist after revoke rollback"
    );
}

#[test]
fn grant_idempotent_no_duplicate_event() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    let conn = journal.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM journal_events WHERE kind = 'OperationGrantChanged' AND payload_json LIKE '%granted%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "idempotent grant must not duplicate event");
}

#[test]
fn revoke_idempotent_no_duplicate_event() {
    let journal = in_memory_journal();
    grant_operation(&journal, "Cli", "harness.op").unwrap();
    revoke_operation(&journal, "Cli", "harness.op").unwrap();
    revoke_operation(&journal, "Cli", "harness.op").unwrap();
    let conn = journal.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM journal_events WHERE kind = 'OperationGrantChanged' AND payload_json LIKE '%revoked%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "idempotent revoke must not duplicate event");
}
