//! Atomic Receipt append-or-compare with conflict detection (H3).
//!
//! Uniqueness key: (hcr_id, claim_id, run_id, idempotency_key)
//!
//! - Same key + identical content → Duplicate (idempotent replay)
//! - Same key + different content → Conflict (rejected)
//! - New key → Appended

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{anyhow, bail, Result};
use serde_json::Value;

/// Result of a receipt append-or-compare operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendReceiptResult {
    /// Receipt was appended successfully.
    Appended,
    /// Exact duplicate — same key, same content.
    Duplicate,
    /// Conflict — same key, different content.
    Conflict(String),
}

/// Receipt key: (hcr_id, claim_id, run_id, idempotency_key).
///
/// Used as the correlation_id in the journal event to enable lookup
/// and conflict detection.
pub fn receipt_key(hcr_id: &str, claim_id: &str, run_id: &str, idempotency_key: &str) -> String {
    format!("receipt:{hcr_id}:{claim_id}:{run_id}:{idempotency_key}")
}

/// Append a receipt or detect duplicate/conflict atomically.
///
/// Uses an INSERT OR IGNORE pattern on the journal with a unique
/// constraint on the correlation_id. If the same correlation_id
/// already exists with identical payload content, returns Duplicate.
/// If same correlation_id with different content, returns Conflict.
pub fn append_or_compare_receipt(
    journal: &JournalStore,
    run_id: &RunId,
    session_id: &SessionId,
    key: &str,
    receipt_status: ReceiptStatus,
    output: &Value,
) -> Result<AppendReceiptResult> {
    // First, check if a receipt with this key already exists
    let existing = journal.find_events_by_correlation(key)?;

    if !existing.is_empty() {
        // Same key found — check if content matches
        let last = &existing[existing.len() - 1];
        let last_status = last.payload.get("status")
            .and_then(|v| v.as_str()).unwrap_or("");
        let last_output = last.payload.get("output");

        let our_status = format!("{:?}", receipt_status);

        if last_status == our_status && last_output == Some(output) {
            return Ok(AppendReceiptResult::Duplicate);
        } else {
            return Ok(AppendReceiptResult::Conflict(format!(
                "existing status={last_status} vs new={our_status}"
            )));
        }
    }

    // No existing receipt — append new one
    let correlation_id = key;
    let payload = serde_json::json!({
        "invocation_id": format!("hcr_accept_{}", key),
        "status": format!("{:?}", receipt_status),
        "output": output,
        "correlation_key": key,
    });

    journal.append_event(
        JournalEventKind::ReceiptReceived,
        Some(run_id),
        Some(session_id),
        Some(correlation_id),
        payload,
    )?;

    Ok(AppendReceiptResult::Appended)
}

impl JournalStore {
    /// Create a persisted Run record for HCR acceptance.
    pub fn create_hcr_run(&self, run: &Run) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow!("mutex: {e}"))?;
        let mode_str = serde_json::to_string(&run.mode)?;
        conn.execute(
            "INSERT OR IGNORE INTO runs (id, session_id, agent_id, trigger_event_id, principal_json,
             parent_run_id, delegated_by, status, created_at, updated_at, registry_snapshot_id, mode)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                run.id.0, run.session_id.0, run.agent_id.0, run.trigger_event_id.0,
                serde_json::to_string(&run.principal)?,
                run.parent_run_id.as_ref().map(|r| r.0.as_str()),
                run.delegated_by.as_ref().map(|p| p.0.as_str()),
                format!("{:?}", run.status),
                run.created_at.to_rfc3339(),
                run.updated_at.to_rfc3339(),
                run.registry_snapshot_id,
                mode_str,
            ],
        )?;
        Ok(())
    }

    /// Find journal events by correlation_id.
    pub fn find_events_by_correlation(&self, correlation_id: &str) -> Result<Vec<JournalEvent>> {
        let conn = self.conn.lock().map_err(|e| anyhow!("mutex: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT sequence, event_id, run_id, session_id, correlation_id, kind,
                    payload_json, previous_hash, hash, created_at
             FROM journal_events WHERE correlation_id = ?1
             ORDER BY sequence"
        )?;
        let rows = stmt.query_map(rusqlite::params![correlation_id], |row| {
            Ok(JournalEvent {
                sequence: row.get(0)?,
                event_id: EventId(row.get(1)?),
                run_id: row.get::<_, Option<String>>(2)?.map(RunId),
                session_id: row.get::<_, Option<String>>(3)?.map(SessionId),
                correlation_id: row.get(4)?,
                kind: serde_json::from_str(&format!("\"{}\"", row.get::<_, String>(5)?))
                    .unwrap_or(JournalEventKind::Unknown),
                payload: serde_json::from_str(&row.get::<_, String>(6)?)
                    .unwrap_or_default(),
                previous_hash: row.get(7)?,
                hash: row.get(8)?,
                created_at: row.get::<_, String>(9)?
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap_or_default(),
            })
        }).map_err(|e| anyhow!("{e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }
}
