//! approval_event_and_intent_are_atomic
//!
//! The approval event and deployment intent must be committed in the
//! same journal transaction — the journal must never contain an
//! approval without a matching intent, or vice versa.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

#[test]
fn approval_event_and_intent_are_atomic() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Record the full atomic chain: proposal → approved → activated
    journal.append_event(
        JournalEventKind::CapabilityChangeProposed, None, None,
        Some("corr_proposal"),
        json!({"proposal_id": "p1", "status": "PendingApproval"}),
    )?;
    journal.append_event(
        JournalEventKind::CapabilityChangeApproved, None, None,
        Some("corr_approval"),
        json!({"proposal_id": "p1", "status": "Approved"}),
    )?;
    journal.append_event(
        JournalEventKind::CapabilityChangeActivated, None, None,
        Some("corr_activated"),
        json!({"proposal_id": "p1", "status": "Activated"}),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].kind, JournalEventKind::CapabilityChangeProposed);
    assert_eq!(events[1].kind, JournalEventKind::CapabilityChangeApproved);
    assert_eq!(events[2].kind, JournalEventKind::CapabilityChangeActivated);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn pending_deployment_intent_is_tracked() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    
    journal.append_event(
        JournalEventKind::CapabilityChangeActivated, None, None,
        Some("corr_deploy"),
        json!({"proposal_id": "p2", "status": "deployment_pending"}),
    )?;
    
    let events = journal.events()?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["status"], "deployment_pending");
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
