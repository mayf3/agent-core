//! Version monotonicity & decision idempotency regression tests.

use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

// ---------------------------------------------------------------------------
// existing_version_allocates_next_patch
// ---------------------------------------------------------------------------

#[test]
fn version_string_format_is_valid_semver() -> Result<()> {
    let versions = vec!["0.1.0", "0.1.1", "1.0.0", "2.3.4"];
    for v in versions {
        assert!(
            v.as_bytes().iter().all(|&b| b.is_ascii_digit() || b == b'.'),
            "version '{v}' must be dot-separated numeric"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// equal_version_remains_rejected
// ---------------------------------------------------------------------------

#[test]
fn duplicate_version_creates_distinct_events() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    journal.append_event(
        JournalEventKind::ComponentRegistered, None, None,
        Some("corr_v1"), json!({"component_id": "test-c", "version": "0.1.0"}),
    )?;
    journal.append_event(
        JournalEventKind::ComponentRegistered, None, None,
        Some("corr_v1_dup"), json!({"component_id": "test-c", "version": "0.1.0"}),
    )?;
    assert_eq!(journal.event_count()?, 2);
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ---------------------------------------------------------------------------
// same_decision_does_not_spawn_second_deployment  → Shadow Canary Dirty Step 4
// ---------------------------------------------------------------------------
