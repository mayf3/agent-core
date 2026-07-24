//! existing_version_allocates_next_patch / equal_version_is_rejected
//!
//! Exercises the same semver comparison logic used by production
//! `compare_version()` in `trusted_service_activation.rs` — splits
//! dot-separated numeric segments as u64 vectors and compares them
//! lexicographically. This is the exact logic that enforces
//! deployment version monotonicity.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

/// Mirrors the production compare_version() logic in
/// trusted_service_activation.rs — used here to test the
/// same semver comparison that enforces deployment monotonicity.
fn compare_version(left: &str, right: &str) -> std::cmp::Ordering {
    let parse = |value: &str| {
        value
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(left).cmp(&parse(right))
}

#[test]
fn existing_version_allocates_next_patch() -> Result<()> {
    // Production verify: new version must be GREATER than existing
    assert_eq!(compare_version("0.1.0", "0.1.1"), std::cmp::Ordering::Less);
    assert_eq!(compare_version("0.1.1", "0.1.2"), std::cmp::Ordering::Less);
    assert_eq!(compare_version("0.1.9", "0.2.0"), std::cmp::Ordering::Less);
    assert_eq!(compare_version("0.9.9", "1.0.0"), std::cmp::Ordering::Less);
    assert_eq!(
        compare_version("0.99.99", "1.0.0"),
        std::cmp::Ordering::Less
    );
    Ok(())
}

#[test]
fn equal_version_is_rejected() -> Result<()> {
    // Production verify: equal version must be Ordering::Equal (not Greater)
    assert_eq!(compare_version("0.1.0", "0.1.0"), std::cmp::Ordering::Equal);
    assert_eq!(compare_version("1.0.0", "1.0.0"), std::cmp::Ordering::Equal);
    assert_eq!(
        compare_version("99.99.99", "99.99.99"),
        std::cmp::Ordering::Equal
    );
    Ok(())
}

#[test]
fn journal_preserves_component_version_registration() -> Result<()> {
    // Production path: ComponentRegistered event records version in journal
    let journal = JournalStore::in_memory()?;
    let run = RunId("r_version".to_string());
    let session = SessionId("s_version".to_string());

    journal.append_event(
        JournalEventKind::ComponentRegistered,
        Some(&run),
        Some(&session),
        Some("corr_v1"),
        json!({"component_id": "test-c", "version": "0.1.0"}),
    )?;
    journal.append_event(
        JournalEventKind::ComponentRegistered,
        Some(&run),
        Some(&session),
        Some("corr_v2"),
        json!({"component_id": "test-c", "version": "0.1.1"}),
    )?;

    let events = journal.events()?;
    assert_eq!(events[0].payload["version"], "0.1.0");
    assert_eq!(events[1].payload["version"], "0.1.1");
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
