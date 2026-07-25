//! Journal event helpers for coding‑task invocations.
//!
//! These were originally part of `invocable.rs` but are governance‑level
//! journal records, not product‑specific manifest construction.  They
//! remain in the Kernel alongside `handler.rs` and must not be moved to
//! the Coding Harness.

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::Result;

/// Append an `InvocationProposed` journal event.
pub fn append_invocation_proposed(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    intent: &InvocationIntent,
) -> Result<()> {
    journal.append_event(
        JournalEventKind::InvocationProposed,
        Some(&run.id),
        Some(&session.id),
        Some(&intent.invocation_id.0),
        serde_json::json!({
            "invocation_id": intent.invocation_id.0,
            "operation": intent.operation,
            "idempotency_key": intent.idempotency_key,
        }),
    )?;
    Ok(())
}

/// Append an `InvocationApproved` journal event.
pub fn append_invocation_approved(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    approved: &ApprovedInvocation,
) -> Result<()> {
    journal.append_event(
        JournalEventKind::InvocationApproved,
        Some(&run.id),
        Some(&session.id),
        Some(&approved.intent().invocation_id.0),
        serde_json::json!({
            "invocation_id": approved.intent().invocation_id.0,
            "operation": approved.intent().operation,
            "decision_id": approved.decision_id,
        }),
    )?;
    Ok(())
}
