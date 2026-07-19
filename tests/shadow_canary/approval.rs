//! Approval & atomicity regression tests.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

// ---------------------------------------------------------------------------
// approval_event_and_intent_commit_atomically
// ---------------------------------------------------------------------------

#[test]
fn journal_preserves_approval_event_sequence() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    journal.append_event(
        JournalEventKind::CapabilityChangeProposed, None, None,
        Some("corr_proposal"), json!({"proposal_id": "p1", "status": "PendingApproval"}),
    )?;
    journal.append_event(
        JournalEventKind::CapabilityChangeApproved, None, None,
        Some("corr_approval"), json!({"proposal_id": "p1", "status": "Approved"}),
    )?;
    journal.append_event(
        JournalEventKind::CapabilityChangeActivated, None, None,
        Some("corr_activation"), json!({"proposal_id": "p1", "status": "Activated"}),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].kind, JournalEventKind::CapabilityChangeProposed);
    assert_eq!(events[1].kind, JournalEventKind::CapabilityChangeApproved);
    assert_eq!(events[2].kind, JournalEventKind::CapabilityChangeActivated);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ---------------------------------------------------------------------------
// connector_accepts_deployment_pending  → Shadow Canary Step 5
//   simulateCardApproval() reçoit deployment_pending du Kernel
//   et le Connector ACK sans attendre.
// ---------------------------------------------------------------------------
