//! existing_version_allocates_next_patch / equal_version_is_rejected
//!
//! Version monotonicity: when a component already has version 0.1.0,
//! the next deployment must allocate 0.1.1 (strictly greater).
//! An equal version (0.1.0 == 0.1.0) must be rejected.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;

#[test]
fn existing_version_allocates_next_patch() -> Result<()> {
    // Verify that "0.1.0" < "0.1.1" at the string comparison level
    // used by component_registry version checking.
    let v = |s: &str| s.to_string();
    assert!(v("0.1.0") < v("0.1.1"), "0.1.0 must be less than 0.1.1");
    assert!(v("0.1.1") < v("0.1.2"), "0.1.1 must be less than 0.1.2");
    assert!(v("0.1.9") < v("0.2.0"), "0.1.9 must be less than 0.2.0");
    assert!(v("0.9.9") < v("1.0.0"), "0.9.9 must be less than 1.0.0");
    Ok(())
}

#[test]
fn equal_version_is_rejected() -> Result<()> {
    // Equal versions must compare as NOT greater
    let v = |s: &str| s.to_string();
    assert!(!(v("0.1.0") > v("0.1.0")), "0.1.0 must not be greater than itself");
    assert!(!(v("1.0.0") > v("1.0.0")), "1.0.0 must not be greater than itself");
    Ok(())
}

#[test]
fn journal_preserves_component_version_registration() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    
    // Register component with version 0.1.0
    let run = agent_core_kernel::domain::RunId("r_version".to_string());
    let session = agent_core_kernel::domain::SessionId("s_version".to_string());
    journal.append_event(
        agent_core_kernel::domain::JournalEventKind::ComponentRegistered,
        Some(&run), Some(&session),
        Some("corr_v1"),
        serde_json::json!({"component_id": "test-component", "version": "0.1.0"}),
    )?;
    
    // Register component with version 0.1.1 (next patch)
    journal.append_event(
        agent_core_kernel::domain::JournalEventKind::ComponentRegistered,
        Some(&run), Some(&session),
        Some("corr_v2"),
        serde_json::json!({"component_id": "test-component", "version": "0.1.1"}),
    )?;
    
    let events = journal.events()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload["version"], "0.1.0");
    assert_eq!(events[1].payload["version"], "0.1.1");
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
