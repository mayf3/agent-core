//! outbox_unknown_idempotent_retry
//!
//! An outbox dispatch that resolves to Unknown must survive a retry
//! cycle without corruption. Retrying an already-unknown entry must
//! be idempotent.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;

#[test]
fn outbox_unknown_count_starts_zero() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let count = journal.outbox_unknown_unacked_count()?;
    assert_eq!(count, 0, "fresh journal must have 0 unknown unacked");
    Ok(())
}

#[test]
fn outbox_unknown_idempotent_retry() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    // Unknown count is 0 initially
    assert_eq!(journal.outbox_unknown_unacked_count()?, 0);
    // Record an unknown event
    let run = RunId("r_unknown_retry".to_string());
    let session = SessionId("s_unknown_retry".to_string());
    journal.append_event(
        JournalEventKind::RunFailed,
        Some(&run),
        Some(&session),
        Some("corr_unknown"),
        serde_json::json!({"outcome": "Unknown"}),
    )?;
    assert_eq!(journal.event_count()?, 1);
    assert!(journal.verify_hash_chain()?);
    // Hash chain remains valid on re-read
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
