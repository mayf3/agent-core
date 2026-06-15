use super::queue::append_event_tx;
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde_json::json;

impl JournalStore {
    pub fn unknown_invocations(&self) -> Result<Vec<UnknownInvocation>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT d.correlation_id, d.run_id, d.session_id, MIN(d.created_at)
             FROM journal_events d
             WHERE d.kind = 'DispatchStarted'
               AND d.correlation_id IS NOT NULL
               AND NOT EXISTS (
                 SELECT 1 FROM journal_events r
                 WHERE r.kind = 'ReceiptReceived'
                   AND r.correlation_id = d.correlation_id
               )
               AND NOT EXISTS (
                 SELECT 1 FROM journal_events u
                 WHERE u.kind = 'OutboxDispatchUnknown'
                   AND u.correlation_id = d.correlation_id
               )
             GROUP BY d.correlation_id, d.run_id, d.session_id
             ORDER BY MIN(d.sequence)",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(UnknownInvocation {
                invocation_id: row.get(0)?,
                run_id: row.get::<_, Option<String>>(1)?.map(RunId),
                session_id: row.get::<_, Option<String>>(2)?.map(SessionId),
                first_dispatch_at: parse_time(row.get::<_, String>(3)?)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Recover `DispatchStarted` without terminal receipt/unknown events.
    ///
    /// Writes `OutboxDispatchUnknown` as the terminal fact for each candidate
    /// (Task 4.1) and clears stale `dispatching` projection rows whose journal
    /// is already terminal without appending duplicate events (Task 4.2).
    ///
    /// Safety:
    /// - never calls adapter / dispatch_once
    /// - never writes ReceiptReceived for unknown invocations
    /// - never writes RunCompleted or RunFailed
    /// - never returns a row to `pending` or `retryable_failed`
    pub fn recover_unknown_invocations(&self) -> Result<usize> {
        let mut recovered = 0usize;

        let candidates = self.unknown_invocations()?;
        for invocation in &candidates {
            let mut conn = self
                .conn
                .lock()
                .map_err(|_| anyhow!("journal mutex poisoned"))?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            append_event_tx(
                &tx,
                JournalEventKind::OutboxDispatchUnknown,
                invocation.run_id.as_ref(),
                invocation.session_id.as_ref(),
                Some(&invocation.invocation_id),
                json!({
                    "invocation_id": invocation.invocation_id,
                    "error": "dispatch_started_without_receipt",
                    "recovered": true,
                    "status": OutboxDispatchStatus::Unknown.as_str(),
                }),
            )?;
            tx.execute(
                "UPDATE outbox_dispatches
                 SET status = ?1, locked_by = NULL, locked_until = NULL, updated_at = ?2
                 WHERE invocation_id = ?3",
                params![
                    OutboxDispatchStatus::Unknown.as_str(),
                    Utc::now().to_rfc3339(),
                    invocation.invocation_id,
                ],
            )?;
            tx.commit()?;
            recovered += 1;
        }

        let stale = self.stale_dispatching_with_terminal_journal()?;
        for invocation_id in stale {
            let mut conn = self
                .conn
                .lock()
                .map_err(|_| anyhow!("journal mutex poisoned"))?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE outbox_dispatches
                 SET status = ?1, locked_by = NULL, locked_until = NULL, updated_at = ?2
                 WHERE invocation_id = ?3 AND status = ?4",
                params![
                    OutboxDispatchStatus::Unknown.as_str(),
                    Utc::now().to_rfc3339(),
                    invocation_id,
                    OutboxDispatchStatus::Dispatching.as_str(),
                ],
            )?;
            tx.commit()?;
            recovered += 1;
        }

        Ok(recovered)
    }

    /// Projection rows still flagged `dispatching` with an expired lease while
    /// the Journal already contains a terminal event. These must be repaired
    /// without appending duplicate journal events.
    fn stale_dispatching_with_terminal_journal(&self) -> Result<Vec<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let now_text = Utc::now().to_rfc3339();
        let mut stmt = conn.prepare(
            "SELECT od.invocation_id
             FROM outbox_dispatches od
             WHERE od.status = ?1
               AND (od.locked_until IS NULL OR od.locked_until <= ?2)
               AND EXISTS (
                 SELECT 1 FROM journal_events je
                 WHERE je.correlation_id = od.invocation_id
                   AND je.kind IN ('OutboxDispatchUnknown', 'ReceiptReceived')
               )",
        )?;
        let rows = stmt.query_map(
            params![OutboxDispatchStatus::Dispatching.as_str(), now_text.as_str()],
            |row| Ok(row.get::<_, String>(0)?),
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}
