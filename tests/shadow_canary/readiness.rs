//! Readiness & event endpoint regression tests.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::event_observe::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

// ---------------------------------------------------------------------------
// valid_empty_event_page_marks_ready
// ---------------------------------------------------------------------------

#[test]
fn valid_empty_event_page_marks_ready() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let resp = journal.observe_events(&EventObserveQuery {
        after_sequence: None, limit: 100, ..Default::default()
    })?;
    assert!(resp.events.is_empty());
    assert!(!resp.has_more);
    assert_eq!(resp.next_cursor, 0);
    assert_eq!(resp.schema_version, OBSERVE_SCHEMA_VERSION);
    Ok(())
}

#[test]
fn empty_page_with_cursor_zero() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let r1 = journal.observe_events(&EventObserveQuery {
        after_sequence: None, limit: 100, ..Default::default()
    })?;
    let r2 = journal.observe_events(&EventObserveQuery {
        after_sequence: Some(0), limit: 100, ..Default::default()
    })?;
    assert_eq!(r1.events.len(), r2.events.len());
    assert_eq!(r1.has_more, r2.has_more);
    Ok(())
}

// ---------------------------------------------------------------------------
// events_endpoint_remains_concurrent_during_deployment
// ---------------------------------------------------------------------------

#[test]
fn events_available_during_deployment_intent() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let run = RunId("r_conc".to_string());
    let session = SessionId("s_conc".to_string());

    for i in 0..3 {
        journal.append_event(
            JournalEventKind::RunStarted, Some(&run), Some(&session),
            Some(&format!("corr_{i}")), json!({"idx": i}),
        )?;
    }
    journal.append_event(
        JournalEventKind::CapabilityChangeProposed, None, None,
        Some("corr_deploy"), json!({"proposal_id": "p1", "status": "PendingApproval"}),
    )?;

    let resp = journal.observe_events(&EventObserveQuery {
        after_sequence: None, limit: 100, ..Default::default()
    })?;
    assert_eq!(resp.events.len(), 4);
    assert!(!resp.has_more);
    Ok(())
}

// ---------------------------------------------------------------------------
// invalid_token_never_marks_ready  → Shadow Canary doctor (port auth check)
// kernel_unavailable_never_marks_ready  → Shadow Canary doctor (port health)
// ---------------------------------------------------------------------------
