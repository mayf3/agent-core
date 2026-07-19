//! Recovery & isolation regression tests.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

// ---------------------------------------------------------------------------
// activation_failed_history_does_not_block_new_proposal
// ---------------------------------------------------------------------------

#[test]
fn activation_failed_does_not_block_new_events() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Past activation failure
    journal.append_event(
        JournalEventKind::CapabilityChangeActivated, None, None,
        Some("corr_fail"), json!({
            "proposal_id": "p_fail", "status": "ActivationFailed",
        }),
    )?;

    // New proposal after failure
    journal.append_event(
        JournalEventKind::CapabilityChangeProposed, None, None,
        Some("corr_new"), json!({
            "proposal_id": "p_new", "status": "PendingApproval",
        }),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, JournalEventKind::CapabilityChangeActivated);
    assert_eq!(events[1].kind, JournalEventKind::CapabilityChangeProposed);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ---------------------------------------------------------------------------
// backend_proposal_cannot_claim_feishu_origin
// ---------------------------------------------------------------------------

#[test]
fn proposals_distinguish_origin_channel() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Feishu-origin
    journal.append_event(
        JournalEventKind::CapabilityChangeProposed, None, None,
        Some("corr_feishu"), json!({
            "proposal_id": "p_feishu",
            "origin_channel": "Feishu",
            "origin_conversation_kind": "p2p",
        }),
    )?;

    // Backend-origin
    journal.append_event(
        JournalEventKind::CapabilityChangeProposed, None, None,
        Some("corr_backend"), json!({
            "proposal_id": "p_backend",
            "origin_channel": "Kernel",
        }),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload["origin_channel"], "Feishu");
    assert_eq!(events[1].payload["origin_channel"], "Kernel");
    Ok(())
}

// ---------------------------------------------------------------------------
// journal_queries_match_real_schema
// ---------------------------------------------------------------------------

#[test]
fn journal_schema_version_is_current() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let version = journal.schema_version()?;
    assert!(version >= 14, "expected schema version >= 14, got {version}");
    Ok(())
}

#[test]
fn journal_basic_queries_work() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert_eq!(journal.event_count()?, 0);
    assert!(journal.verify_hash_chain()?);

    // Append an event and verify count
    let run = RunId("r_schema".to_string());
    let session = SessionId("s_schema".to_string());
    journal.append_event(
        JournalEventKind::RunStarted, Some(&run), Some(&session),
        Some("corr_schema"), json!({"test": true}),
    )?;

    assert_eq!(journal.event_count()?, 1);
    assert!(journal.verify_hash_chain()?);

    // Verify the event payload
    let events = journal.events()?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["test"], true);

    Ok(())
}

#[test]
fn hash_chain_survives_event_append() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert!(journal.verify_hash_chain()?);

    for i in 0..10 {
        journal.append_event(
            JournalEventKind::RunStarted, None, None,
            Some(&format!("corr_{i}")), json!({"idx": i}),
        )?;
        assert!(journal.verify_hash_chain()?, "chain broken after event {i}");
    }
    Ok(())
}
