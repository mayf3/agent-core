//! Support smoke tests: journal schema, hash chain, event observation.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::event_observe::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

#[test]
fn journal_schema_version_is_current() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert!(journal.schema_version()? >= 14);
    Ok(())
}

#[test]
fn hash_chain_survives_events() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert!(journal.verify_hash_chain()?);
    for i in 0..5 {
        journal.append_event(
            JournalEventKind::RunStarted, None, None,
            Some(&format!("corr_{i}")), json!({"idx": i}),
        )?;
        assert!(journal.verify_hash_chain()?);
    }
    Ok(())
}

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
fn undelivered_ingress_query_works() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert!(journal.undelivered_ingress_events()?.is_empty());
    Ok(())
}

#[test]
fn outbox_health_queries_work() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert_eq!(journal.outbox_unknown_unacked_count()?, 0);
    Ok(())
}

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
        Some("corr_deploy"), json!({"status": "PendingApproval"}),
    )?;
    
    let resp = journal.observe_events(&EventObserveQuery {
        after_sequence: None, limit: 100, ..Default::default()
    })?;
    assert_eq!(resp.events.len(), 4);
    Ok(())
}
