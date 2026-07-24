//! connector_accepts_deployment_pending / callback_ack_before_deployment_finishes
//!
//! These are Connector-side behaviors verified by the Shadow Canary:
//!
//! connector_accepts_deployment_pending:
//!   The Kernel returns deployment_pending for async deployments.
//!   The Connector executeProposalDecision() handles this status
//!   by returning an immediate ACK without waiting for activation.
//!
//! callback_ack_before_deployment_finishes:
//!   The POST to the decision endpoint returns before the background
//!   deployment thread finishes. The Connector does NOT poll or block.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

#[test]
fn journal_records_deployment_pending_status() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    journal.append_event(
        JournalEventKind::CapabilityChangeActivated,
        None,
        None,
        Some("corr_callback"),
        json!({
            "proposal_id": "p_callback",
            "status": "deployment_pending",
        }),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["status"], "deployment_pending");
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
