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

    pub fn recover_unknown_invocations(&self) -> Result<usize> {
        let unknown = self.unknown_invocations()?;
        for invocation in &unknown {
            self.append_event(
                JournalEventKind::ReceiptReceived,
                invocation.run_id.as_ref(),
                invocation.session_id.as_ref(),
                Some(&invocation.invocation_id),
                json!({
                    "status": "Unknown",
                    "external_ref": null,
                    "output_kind": "unknown",
                    "recovered": true,
                }),
            )?;
            if let Some(run_id) = &invocation.run_id {
                self.fail_run(run_id)?;
                self.append_event(
                    JournalEventKind::RunCompleted,
                    Some(run_id),
                    invocation.session_id.as_ref(),
                    None,
                    json!({ "status": "Failed", "reason": "unknown_invocation_recovered" }),
                )?;
            }
            let conn = self
                .conn
                .lock()
                .map_err(|_| anyhow!("journal mutex poisoned"))?;
            conn.execute(
                "UPDATE outbox_dispatches
                 SET status = 'unknown', locked_by = NULL, locked_until = NULL, updated_at = ?1
                 WHERE invocation_id = ?2",
                params![Utc::now().to_rfc3339(), invocation.invocation_id],
            )?;
        }
        Ok(unknown.len())
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
