use super::queue::append_event_tx;
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde_json::json;

/// The terminal Journal fact a stale `dispatching` projection should be
/// reconciled against. The recovery uses this to pick the projection target
/// instead of unconditionally marking rows `unknown`.
enum TerminalFact {
    /// `ReceiptReceived` with `status = Succeeded` → projection `succeeded`.
    Succeeded,
    /// `ReceiptReceived` with `status = Failed` → projection `failed`.
    Failed,
    /// `OutboxDispatchUnknown`, or a `ReceiptReceived` whose status cannot be
    /// parsed (defensive) → projection `unknown`.
    Unknown,
}

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
        for (invocation_id, fact) in stale {
            let target = match fact {
                TerminalFact::Succeeded => OutboxDispatchStatus::Succeeded,
                TerminalFact::Failed => OutboxDispatchStatus::Failed,
                TerminalFact::Unknown => OutboxDispatchStatus::Unknown,
            };
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
                    target.as_str(),
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
    /// without appending duplicate journal events. The projection target is
    /// derived from whichever terminal fact the Journal carries:
    /// `ReceiptReceived(status=Succeeded)` → `succeeded`,
    /// `ReceiptReceived(status=Failed)` → `failed`,
    /// `OutboxDispatchUnknown` (or anything unparseable) → `unknown`.
    fn stale_dispatching_with_terminal_journal(&self) -> Result<Vec<(String, TerminalFact)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let now_text = Utc::now().to_rfc3339();
        let mut stmt = conn.prepare(
            "SELECT od.invocation_id, (
               SELECT je.kind FROM journal_events je
               WHERE je.correlation_id = od.invocation_id
                 AND je.kind IN ('OutboxDispatchUnknown', 'ReceiptReceived')
               ORDER BY je.sequence DESC LIMIT 1
             ) AS terminal_kind,
             (
               SELECT je.payload_json FROM journal_events je
               WHERE je.correlation_id = od.invocation_id
                 AND je.kind = 'ReceiptReceived'
               ORDER BY je.sequence DESC LIMIT 1
             ) AS receipt_payload
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
            |row| {
                let invocation_id: String = row.get(0)?;
                let terminal_kind: Option<String> = row.get(1)?;
                let receipt_payload: Option<String> = row.get(2)?;
                Ok((invocation_id, terminal_kind, receipt_payload))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (invocation_id, terminal_kind, receipt_payload) = row?;
            let fact = classify_terminal_fact(&terminal_kind, receipt_payload.as_deref())?;
            out.push((invocation_id, fact));
        }
        Ok(out)
    }
}

/// Map the terminal Journal fact onto a projection target.
///
/// Ordering rules (PR6):
/// - `OutboxDispatchUnknown` → `Unknown` (always, regardless of any receipt).
/// - `ReceiptReceived` → inspect `payload.status`:
///   - `"Succeeded"` → `Succeeded`
///   - `"Failed"` → `Failed`
///   - anything else (missing, unparseable) → `Unknown` (defensive; never
///     promote an ambiguous receipt to `succeeded`).
/// - No terminal kind → treat as `Unknown` (the caller's EXISTS guard should
///   prevent this, but we stay safe).
fn classify_terminal_fact(
    terminal_kind: &Option<String>,
    receipt_payload: Option<&str>,
) -> Result<TerminalFact> {
    match terminal_kind.as_deref() {
        Some("OutboxDispatchUnknown") => Ok(TerminalFact::Unknown),
        Some("ReceiptReceived") => {
            let status: String = receipt_payload
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|payload| {
                    payload
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            match status.as_str() {
                "Succeeded" => Ok(TerminalFact::Succeeded),
                "Failed" => Ok(TerminalFact::Failed),
                _ => Ok(TerminalFact::Unknown),
            }
        }
        _ => Ok(TerminalFact::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_unknown_kind_routes_to_unknown() {
        let fact = classify_terminal_fact(&None, None).expect("classify ok");
        assert!(matches!(fact, TerminalFact::Unknown));
    }

    #[test]
    fn classify_receipt_routes_by_status() -> Result<()> {
        assert!(matches!(
            classify_terminal_fact(
                &Some("ReceiptReceived".to_string()),
                Some(r#"{"status":"Succeeded"}"#),
            )?,
            TerminalFact::Succeeded
        ));
        assert!(matches!(
            classify_terminal_fact(
                &Some("ReceiptReceived".to_string()),
                Some(r#"{"status":"Failed"}"#),
            )?,
            TerminalFact::Failed
        ));
        // Unparseable receipt status stays unknown — never promoted to succeeded.
        assert!(matches!(
            classify_terminal_fact(
                &Some("ReceiptReceived".to_string()),
                Some(r#"{"status":""}"#),
            )?,
            TerminalFact::Unknown
        ));
        assert!(matches!(
            classify_terminal_fact(&Some("ReceiptReceived".to_string()), None)?,
            TerminalFact::Unknown
        ));
        Ok(())
    }

    #[test]
    fn classify_outbox_unknown_always_routes_to_unknown() -> Result<()> {
        assert!(matches!(
            classify_terminal_fact(&Some("OutboxDispatchUnknown".to_string()), None)?,
            TerminalFact::Unknown
        ));
        Ok(())
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
