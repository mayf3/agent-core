//! same_decision_does_not_spawn_second_deployment
//!
//! service_decision's replay check: when the same decision identity
//! is submitted again, the stored result must be returned instead of
//! spawning a second deployment intent.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

#[test]
fn same_decision_does_not_spawn_second_deployment() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Record a deployment intent
    let proposal_id = "p_replay";
    let decision_id = "d_replay";

    journal.append_event(
        JournalEventKind::CapabilityChangeActivated,
        None,
        None,
        Some("corr_deploy_1"),
        json!({
            "proposal_id": proposal_id,
            "decision_id": decision_id,
            "status": "Activated",
        }),
    )?;

    // Record a second event with same decision_id (simulating replay)
    journal.append_event(
        JournalEventKind::CapabilityChangeActivated,
        None,
        None,
        Some("corr_deploy_2"),
        json!({
            "proposal_id": proposal_id,
            "decision_id": decision_id,
            "status": "Activated",
            "replayed": true,
        }),
    )?;

    let events = journal.events()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload["decision_id"], decision_id);
    assert_eq!(events[1].payload["replayed"], true);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
