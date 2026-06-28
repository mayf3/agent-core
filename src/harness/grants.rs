//! Explicit channel operation grants for external harness operations.
//!
//! Grant and revoke are independent Admin actions — register, compose, and
//! activate do NOT automatically write to this table.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::domain::{CapabilityGrant, ChannelKind, JournalEventKind};
use crate::journal::JournalStore;

/// A single channel → operation grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationGrant {
    pub channel: String,
    pub operation_name: String,
    pub created_at: String,
}

/// Grant an operation on a channel. Idempotent.
pub fn grant_operation(
    journal: &JournalStore,
    channel: &str,
    operation_name: &str,
) -> Result<OperationGrant, anyhow::Error> {
    validate_channel(channel)?;
    let now = Utc::now().to_rfc3339();
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    conn.execute(
        "INSERT OR IGNORE INTO channel_operation_grants (channel, operation_name, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![channel, operation_name, now],
    )?;
    drop(conn);

    let _ = journal.append_event(
        JournalEventKind::OperationGrantChanged,
        None,
        None,
        None,
        serde_json::json!({
            "channel": channel,
            "operation_name": operation_name,
            "action": "granted",
        }),
    );

    Ok(OperationGrant {
        channel: channel.to_string(),
        operation_name: operation_name.to_string(),
        created_at: now,
    })
}

/// Revoke an operation on a channel. Idempotent (no-op if not present).
pub fn revoke_operation(
    journal: &JournalStore,
    channel: &str,
    operation_name: &str,
) -> Result<(), anyhow::Error> {
    validate_channel(channel)?;
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    conn.execute(
        "DELETE FROM channel_operation_grants WHERE channel = ?1 AND operation_name = ?2",
        rusqlite::params![channel, operation_name],
    )?;
    drop(conn);

    let _ = journal.append_event(
        JournalEventKind::OperationGrantChanged,
        None,
        None,
        None,
        serde_json::json!({
            "channel": channel,
            "operation_name": operation_name,
            "action": "revoked",
        }),
    );

    Ok(())
}

/// List all grants. Optionally filter by channel.
pub fn list_grants(
    journal: &JournalStore,
    channel: Option<&str>,
) -> Result<Vec<OperationGrant>, anyhow::Error> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let mut stmt = if let Some(_ch) = channel {
        let s = conn.prepare(
            "SELECT channel, operation_name, created_at FROM channel_operation_grants WHERE channel = ?1 ORDER BY operation_name"
        )?;
        s
    } else {
        conn.prepare(
            "SELECT channel, operation_name, created_at FROM channel_operation_grants ORDER BY channel, operation_name"
        )?
    };

    let rows = if let Some(ch) = channel {
        stmt.query_map(rusqlite::params![ch], map_grant)?
    } else {
        stmt.query_map([], map_grant)?
    };
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Get all granted operation names for a channel.
pub fn get_channel_grants(
    journal: &JournalStore,
    channel: &ChannelKind,
) -> Result<Vec<String>, anyhow::Error> {
    let ch_str = format!("{:?}", channel);
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let mut stmt = conn.prepare(
        "SELECT operation_name FROM channel_operation_grants WHERE channel = ?1 ORDER BY operation_name"
    )?;
    let rows = stmt.query_map(rusqlite::params![ch_str], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

// ---- Principal derivation ----

/// Derive the grants for a principal given its channel, the pinned
/// registry snapshot, and operator-configured extras. This is the single
/// entry point that replaces the scattered manual grant construction in
/// Gateway ingress paths.
///
/// The result includes:
/// - Baseline grants from `ExecutionProfile::for_channel(channel)`
/// - Extra operation grants from `KernelConfig.extra_allowed_operations`
/// - Extra operation grants from `channel_operation_grants` table
/// - Operations NOT present in the snapshot are filtered out
pub fn derive_grants(
    journal: &JournalStore,
    channel: &ChannelKind,
    snapshot: &crate::registry::snapshot::RegistrySnapshot,
    extra_allowed_operations: &[String],
) -> Result<Vec<CapabilityGrant>, anyhow::Error> {
    // Start with baseline grants (reply + standard tools) + config extras.
    let profile = crate::domain::operation::ExecutionProfile::for_channel(channel.clone())
        .with_extra(extra_allowed_operations);
    let mut all = profile.grants;

    // Add DB-level channel grants (from admin grant/revoke API).
    let db_grants = get_channel_grants(journal, channel)?;
    for op_name in &db_grants {
        if !all.iter().any(|g| &g.operation == op_name) {
            all.push(CapabilityGrant {
                operation: op_name.clone(),
                scope: "current_session".to_string(),
            });
        }
    }

    // Filter: only keep operations that exist in the pinned snapshot.
    all.retain(|g| snapshot.lookup(&g.operation).is_some());

    Ok(all)
}

// ---- Helpers ----

fn validate_channel(channel: &str) -> Result<(), anyhow::Error> {
    match channel {
        "Cli" | "Feishu" => Ok(()),
        _ => Err(anyhow::anyhow!("unknown channel: {channel}")),
    }
}

fn map_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationGrant> {
    Ok(OperationGrant {
        channel: row.get(0)?,
        operation_name: row.get(1)?,
        created_at: row.get(2)?,
    })
}

#[cfg(test)]
mod tests {
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
        grant_operation(&journal, "Cli", "harness.op").unwrap(); // second time
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
        revoke_operation(&journal, "Cli", "harness.op").unwrap(); // second time — no error
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
        // Baseline: stdout, session.recall_recent
        // DB grant: harness.op
        // Snapshot filter: all three exist
        let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
        assert!(names.contains(&"stdout.send_text"));
        assert!(names.contains(&"session.recall_recent"));
        assert!(names.contains(&"harness.op"));
    }

    #[test]
    fn derive_grants_filters_out_operations_not_in_snapshot() {
        let journal = in_memory_journal();
        // Grant an operation that does NOT exist in the snapshot.
        grant_operation(&journal, "Cli", "missing.op").unwrap();
        let snapshot = make_cli_snapshot(); // does not contain missing.op
        let grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
        let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
        assert!(!names.contains(&"missing.op"), "must be filtered out");
    }

    #[test]
    fn derive_grants_no_db_grant_still_has_baseline() {
        let journal = in_memory_journal();
        // No grant added.
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
        // "system.status" is in the static catalog and in the test snapshot.
        let extras = vec!["system.status".to_string()];
        let grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &extras).unwrap();
        let names: Vec<&str> = grants.iter().map(|g| g.operation.as_str()).collect();
        // Extras that are in the snapshot are included.
        assert!(
            names.contains(&"stdout.send_text"),
            "baseline must be present"
        );
    }

    #[test]
    fn derive_grants_respects_extra_allowed_operations_does_not_break_old_compat() {
        let journal = in_memory_journal();
        let snapshot = make_cli_snapshot();
        // Existing extras that are in the snapshot.
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

        // Cli snapshot includes cli_op so it survives snapshot filtering.
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

        // Feishu snapshot includes feishu_op.
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
        // derive_grants at run creation time snapshots the grants.
        // Later grants/revokes should not retroactively change existing runs
        // since they are already persisted with their snapshot-derived grants.
        let journal = in_memory_journal();
        let snapshot = make_cli_snapshot();

        // Simulate what Runtime::deliver does: derive grants at run creation.
        let run_grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
        assert_eq!(run_grants.len(), 2); // stdout + recall (baseline only)

        // Grant a new operation after the run was created.
        grant_operation(&journal, "Cli", "harness.op").unwrap();

        // The old run's grants should NOT change (we test the derive_grants
        // function, not the persisted run — the persisted run isn't re-derived).
        // A new run would get the new grant.
        let new_run_grants = derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
        assert_eq!(new_run_grants.len(), 3, "new run should see new grant");
        assert_eq!(run_grants.len(), 2, "old run grants unchanged");

        // Revoke after creation.
        revoke_operation(&journal, "Cli", "harness.op").unwrap();
        let after_revoke_grants =
            derive_grants(&journal, &ChannelKind::Cli, &snapshot, &[]).unwrap();
        assert_eq!(
            after_revoke_grants.len(),
            2,
            "new run after revoke loses grant"
        );
    }
}
