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
        "model.invocation.started.v0" => JournalEventKind::ModelInvocationStarted,
        "model.invocation.completed.v0" => JournalEventKind::ModelInvocationCompleted,
        "model.invocation.failed.v0" => JournalEventKind::ModelInvocationFailed,
        "ToolCallIssued" => JournalEventKind::ToolCallIssued,
        "ToolCallRejected" => JournalEventKind::ToolCallRejected,
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
        "AssistantReplyDelivered" => JournalEventKind::AssistantReplyDelivered,
        "WorkerJobDead" => JournalEventKind::WorkerJobDead,
        "RunCompleted" => JournalEventKind::RunCompleted,
        "RunFailed" => JournalEventKind::RunFailed,
        // Phase 2 M2d: approval-state kinds. Without these arms the new
        // kinds would silently route to the `Unknown` sentinel and corrupt
        // the hash chain (see tests/m5_parse_kind.rs).
        "ApprovalRequested" => JournalEventKind::ApprovalRequested,
        "ApprovalGranted" => JournalEventKind::ApprovalGranted,
        "ApprovalDenied" => JournalEventKind::ApprovalDenied,
        "ApprovalExpired" => JournalEventKind::ApprovalExpired,
        "HarnessManifestRegistered" => JournalEventKind::HarnessManifestRegistered,
        "HookCallRecorded" => JournalEventKind::HookCallRecorded,
        "RegistrySnapshotActivated" => JournalEventKind::RegistrySnapshotActivated,
        // Phase 2 capability-change-control-plane: proposal lifecycle kinds.
        "CapabilityChangeProposed" => JournalEventKind::CapabilityChangeProposed,
        "CapabilityChangeApproved" => JournalEventKind::CapabilityChangeApproved,
        "CapabilityChangeActivated" => JournalEventKind::CapabilityChangeActivated,
        "CapabilityChangeActivationFailed" => JournalEventKind::CapabilityChangeActivationFailed,
        "CapabilityChangeRejected" => JournalEventKind::CapabilityChangeRejected,
        "CapabilityChangeExpired" => JournalEventKind::CapabilityChangeExpired,
        "deployment.intent.v0" => JournalEventKind::DeploymentIntentRecorded,
        "deployment.receipt.v0" => JournalEventKind::DeploymentReceiptRecorded,
        "component.registered.v0" => JournalEventKind::ComponentRegistered,
        "component.control.intent.v0" => JournalEventKind::ComponentControlIntentRecorded,
        "component.control.receipt.v0" => JournalEventKind::ComponentControlReceiptRecorded,
        "component.disabled.v0" => JournalEventKind::ComponentDisabled,
        "component.rolled_back.v0" => JournalEventKind::ComponentRolledBack,
        "ToolBudgetExhausted" => JournalEventKind::ToolBudgetExhausted,
        "ToolLoopWallClockExceeded" => JournalEventKind::ToolLoopWallClockExceeded,
        "ToolLoopDetected" => JournalEventKind::ToolLoopDetected,
        "ExternalOperationGranted" => JournalEventKind::ExternalOperationGranted,
        "ExternalOperationRevoked" => JournalEventKind::ExternalOperationRevoked,
        "HarnessChangeRequested" => JournalEventKind::HarnessChangeRequested,
        "HcrClaimSucceeded" => JournalEventKind::HcrClaimSucceeded,
        "HcrClaimRejected" => JournalEventKind::HcrClaimRejected,
        "HcrRunCreated" => JournalEventKind::HcrRunCreated,
        "HcrEvidenceRegistered" => JournalEventKind::HcrEvidenceRegistered,
        "HcrSettlementSucceeded" => JournalEventKind::HcrSettlementSucceeded,
        "HcrSettlementFailed" => JournalEventKind::HcrSettlementFailed,
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
