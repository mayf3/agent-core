//! Explicit channel operation grants for external harness operations.
//!
//! Grant and revoke are independent Admin actions — register, compose, and
//! activate do NOT automatically write to this table.

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::domain::{CapabilityGrant, ChannelKind};
use crate::journal::JournalStore;

/// A single channel → operation grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationGrant {
    pub channel: String,
    pub operation_name: String,
    pub created_at: String,
}

/// Outcome of a grant operation — distinguishes changed vs idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantChange {
    Changed(OperationGrant),
    Unchanged,
}

/// Outcome of a revoke operation — distinguishes changed vs idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeChange {
    Changed,
    Unchanged,
}

/// Grant an operation on a channel. Uses a single transaction for state
/// write + Journal event.
pub fn grant_operation(
    journal: &JournalStore,
    channel: &str,
    operation_name: &str,
) -> Result<OperationGrant, anyhow::Error> {
    validate_channel(channel)?;
    let now = Utc::now().to_rfc3339();

    let mut conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let change = grant_operation_in_transaction(&tx, channel, operation_name, &now)?;

    if let GrantChange::Changed(ref grant) = change {
        crate::journal::hash_chain::append_event_in_transaction(
            &tx,
            "OperationGrantChanged",
            &serde_json::to_string(&serde_json::json!({
                "channel": channel,
                "operation_name": operation_name,
                "action": "granted",
            }))?,
            &now,
        )?;
        tx.commit()?;
        drop(conn);
        return Ok(grant.clone());
    }

    // Unchanged — no event.
    tx.commit()?;
    drop(conn);
    Ok(OperationGrant {
        channel: channel.to_string(),
        operation_name: operation_name.to_string(),
        created_at: now,
    })
}

/// Revoke an operation on a channel. Uses a single transaction for state
/// write + Journal event. Idempotent (no-op if not present).
pub fn revoke_operation(
    journal: &JournalStore,
    channel: &str,
    operation_name: &str,
) -> Result<(), anyhow::Error> {
    validate_channel(channel)?;

    let mut conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let change = revoke_operation_in_transaction(&tx, channel, operation_name)?;

    if change == RevokeChange::Changed {
        let now = Utc::now().to_rfc3339();
        crate::journal::hash_chain::append_event_in_transaction(
            &tx,
            "OperationGrantChanged",
            &serde_json::to_string(&serde_json::json!({
                "channel": channel,
                "operation_name": operation_name,
                "action": "revoked",
            }))?,
            &now,
        )?;
    }

    tx.commit()?;
    drop(conn);

    Ok(())
}

/// Transaction-aware grant helper. Does NOT commit or acquire connection lock.
pub fn grant_operation_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    channel: &str,
    operation_name: &str,
    now: &str,
) -> Result<GrantChange, anyhow::Error> {
    validate_channel(channel)?;

    let existing: Option<String> = tx
        .query_row(
            "SELECT created_at FROM channel_operation_grants WHERE channel = ?1 AND operation_name = ?2",
            rusqlite::params![channel, operation_name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    if existing.is_some() {
        return Ok(GrantChange::Unchanged);
    }

    tx.execute(
        "INSERT INTO channel_operation_grants (channel, operation_name, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![channel, operation_name, now],
    )?;

    Ok(GrantChange::Changed(OperationGrant {
        channel: channel.to_string(),
        operation_name: operation_name.to_string(),
        created_at: now.to_string(),
    }))
}

/// Transaction-aware revoke helper. Does NOT commit or acquire connection lock.
pub fn revoke_operation_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    channel: &str,
    operation_name: &str,
) -> Result<RevokeChange, anyhow::Error> {
    validate_channel(channel)?;

    let before: Option<String> = tx
        .query_row(
            "SELECT created_at FROM channel_operation_grants WHERE channel = ?1 AND operation_name = ?2",
            rusqlite::params![channel, operation_name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    if before.is_none() {
        return Ok(RevokeChange::Unchanged);
    }

    tx.execute(
        "DELETE FROM channel_operation_grants WHERE channel = ?1 AND operation_name = ?2",
        rusqlite::params![channel, operation_name],
    )?;

    Ok(RevokeChange::Changed)
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
    let profile = crate::domain::operation::ExecutionProfile::for_channel(channel.clone())
        .with_extra(extra_allowed_operations);
    let mut all = profile.grants;

    let db_grants = get_channel_grants(journal, channel)?;
    for op_name in &db_grants {
        if !all.iter().any(|g| &g.operation == op_name) {
            all.push(CapabilityGrant {
                operation: op_name.clone(),
                scope: "current_session".to_string(),
            });
        }
    }

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
#[path = "grants_tests.rs"]
mod grants_tests;
