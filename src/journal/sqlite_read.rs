//! Read-side helpers for the SQLite Journal store: row decoding, Journal
//! event-kind parsing, and timestamp parsing. Extracted from `sqlite.rs` to
//! keep that file under the 500-line structure limit.

use crate::domain::*;
use chrono::{DateTime, Utc};
use serde_json::json;

/// Decode a `journal_events` row into a `JournalEvent`. Unrecognized `kind`
/// text routes to the `JournalEventKind::Unknown` sentinel (HANDOVER §10) and
/// emits a sanitized `eprintln` (sequence + kind label only — never payload,
/// correlation_id, run_id, session_id, or external content).
pub(crate) fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<JournalEvent> {
    let kind_text: String = row.get(5)?;
    let payload_json: String = row.get(6)?;
    let sequence: i64 = row.get(0)?;
    let kind = parse_kind(&kind_text);
    if matches!(kind, JournalEventKind::Unknown) && kind_text != "Unknown" {
        // Sanitized diagnostic: only the sequence and the unrecognized kind
        // label. This is an operator signal that either external tampering
        // occurred or a future enum variant's read-path was not updated.
        eprintln!(
            "journal: unrecognized event kind {kind_text:?} at sequence {sequence}; routing to Unknown"
        );
    }
    Ok(JournalEvent {
        sequence,
        event_id: EventId(row.get(1)?),
        run_id: row.get::<_, Option<String>>(2)?.map(RunId),
        session_id: row.get::<_, Option<String>>(3)?.map(SessionId),
        correlation_id: row.get(4)?,
        kind,
        payload: serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({})),
        previous_hash: row.get(7)?,
        hash: row.get(8)?,
        created_at: parse_time(row.get::<_, String>(9)?)?,
    })
}

/// Parse a stored `kind` text into a `JournalEventKind`. Unknown kinds
/// (tampering or future-enum drift) route to the `Unknown` sentinel instead
/// of masquerading as `RunCompleted`. See HANDOVER §10.
pub(crate) fn parse_kind(value: &str) -> JournalEventKind {
    match value {
        "IngressAccepted" => JournalEventKind::IngressAccepted,
        "SessionReady" => JournalEventKind::SessionReady,
        "RunStarted" => JournalEventKind::RunStarted,
        "ContextBuilt" => JournalEventKind::ContextBuilt,
        "LlmCompleted" => JournalEventKind::LlmCompleted,
        "InvocationProposed" => JournalEventKind::InvocationProposed,
        "InvocationApproved" => JournalEventKind::InvocationApproved,
        "WorkerJobQueued" => JournalEventKind::WorkerJobQueued,
        "WorkerJobStarted" => JournalEventKind::WorkerJobStarted,
        "WorkerJobSucceeded" => JournalEventKind::WorkerJobSucceeded,
        "WorkerJobFailed" => JournalEventKind::WorkerJobFailed,
        "OutboxQueued" => JournalEventKind::OutboxQueued,
        "OutboxDispatchFailed" => JournalEventKind::OutboxDispatchFailed,
        "OutboxDispatchUnknown" => JournalEventKind::OutboxDispatchUnknown,
        "OutboxDispatchDead" => JournalEventKind::OutboxDispatchDead,
        "DispatchStarted" => JournalEventKind::DispatchStarted,
        "ReceiptReceived" => JournalEventKind::ReceiptReceived,
        "WorkerJobDead" => JournalEventKind::WorkerJobDead,
        "RunCompleted" => JournalEventKind::RunCompleted,
        "RunFailed" => JournalEventKind::RunFailed,
        _ => JournalEventKind::Unknown,
    }
}

/// Parse an RFC3339 timestamp stored as TEXT back into a UTC `DateTime`.
pub(crate) fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
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
