//! Phase 2 M2d follow-up: expire stale `AwaitingApproval` runs.
//!
//! A run paused for human approval should not block forever. When an operator
//! sets a TTL (`KernelConfig.write_approval_ttl_secs`), startup recovery calls
//! [`JournalStore::expire_stale_approvals`], which advances each run paused
//! longer than the TTL to `Failed` with a terminal `ApprovalExpired` Journal
//! fact. Disabled (TTL = 0) by default → the pre-expiry behavior (a paused run
//! waits indefinitely).
//!
//! See `docs/decisions/m2d-durable-approval.md`.

use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
use chrono::Utc;
use serde_json::json;

/// A run that startup recovery will expire (paused too long).
struct StaleApproval {
    run_id: String,
    operation: String,
    requested_at: String,
}

impl JournalStore {
    /// Expire `AwaitingApproval` runs whose `ApprovalRequested` fact is older
    /// than `ttl_secs`. For each: append `ApprovalExpired` and fail the run
    /// (`Failed`). Returns the count expired. `ttl_secs == 0` is a no-op
    /// (disabled).
    ///
    /// Safety: never calls the adapter, never writes `ReceiptReceived`,
    /// `RunCompleted`, or `OutboxQueued`. A run that has already been resumed
    /// (`ApprovalGranted`/`WaitingDispatch`) or denied (`ApprovalDenied`) is no
    /// longer `AwaitingApproval` and is not selected — the join keys off the
    /// live `runs.status`.
    pub fn expire_stale_approvals(&self, ttl_secs: u64) -> Result<usize> {
        if ttl_secs == 0 {
            return Ok(0);
        }
        let cutoff = Utc::now() - chrono::Duration::seconds(ttl_secs as i64);
        let stale = self.stale_approvals_since(&cutoff)?;
        let now = Utc::now().to_rfc3339();
        for approval in &stale {
            self.append_event(
                JournalEventKind::ApprovalExpired,
                Some(&RunId(approval.run_id.clone())),
                None,
                None,
                json!({
                    "operation": approval.operation,
                    "requested_at": approval.requested_at,
                    "expired_at": now,
                    "ttl_secs": ttl_secs,
                }),
            )?;
            self.fail_run(&RunId(approval.run_id.clone()))?;
        }
        Ok(stale.len())
    }

    /// Select `AwaitingApproval` runs whose most recent `ApprovalRequested`
    /// predates `cutoff` and that have no later `ApprovalGranted`/`ApprovalDenied`.
    fn stale_approvals_since(&self, cutoff: &chrono::DateTime<Utc>) -> Result<Vec<StaleApproval>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT e.run_id, e.payload_json, e.created_at
             FROM journal_events e
             JOIN runs ON runs.id = e.run_id
             WHERE e.kind = 'ApprovalRequested'
               AND e.run_id IS NOT NULL
               AND runs.status = 'AwaitingApproval'
               AND e.created_at < ?1
               AND NOT EXISTS (
                   SELECT 1 FROM journal_events d
                   WHERE d.run_id = e.run_id
                     AND d.kind IN ('ApprovalGranted', 'ApprovalDenied', 'ApprovalExpired')
               )",
        )?;
        let cutoff_str = cutoff.to_rfc3339();
        let rows = stmt.query_map([&cutoff_str], |row| {
            let run_id: String = row.get(0)?;
            let payload: serde_json::Value =
                serde_json::from_str::<String>(&row.get::<_, String>(1)?)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(serde_json::json!({}));
            let requested_at: String = row.get(2)?;
            Ok(StaleApproval {
                run_id,
                operation: payload
                    .get("operation")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                requested_at,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}
