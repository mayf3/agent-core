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
    let profile =
        crate::domain::operation::ExecutionProfile::for_channel(channel.clone())
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
