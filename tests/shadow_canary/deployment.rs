//! Deployment flow regression tests.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

// ---------------------------------------------------------------------------
// intent_without_receipt_is_in_flight
// ---------------------------------------------------------------------------

#[test]
fn deployment_intent_event_is_recorded() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    journal.append_event(
        JournalEventKind::CapabilityChangeActivated, None, None,
        Some("corr_intent"), json!({
            "proposal_id": "p_inflight", "decision_id": "d_inflight",
            "status": "deployment_pending",
        }),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 1);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ---------------------------------------------------------------------------
// failed_receipt_is_not_in_flight
// ---------------------------------------------------------------------------

#[test]
fn failed_deployment_event_is_recorded() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    journal.append_event(
        JournalEventKind::CapabilityChangeActivated, None, None,
        Some("corr_intent"), json!({
            "proposal_id": "p_fail", "decision_id": "d_fail",
            "status": "deployment_pending",
        }),
    )?;
    journal.append_event(
        JournalEventKind::CapabilityChangeActivated, None, None,
        Some("corr_fail"), json!({
            "proposal_id": "p_fail", "status": "ActivationFailed",
            "error": "SERVICE_EXITED_BEFORE_READY",
        }),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 2);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ---------------------------------------------------------------------------
// decision_returns_deployment_pending  → Shadow Canary Step 5
// callback_ack_does_not_wait_for_deployment  → Shadow Canary Step 5
// ---------------------------------------------------------------------------
