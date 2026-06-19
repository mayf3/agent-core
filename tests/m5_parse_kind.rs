use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

/// Regression for the `parse_kind` fallback tightening (HANDOVER §10).
///
/// An unrecognized `kind` string must route to the `Unknown` sentinel, not be
/// silently coerced to `RunCompleted`. This test corrupts the kind column of
/// a real event and asserts (a) `events()` still succeeds and reports
/// `Unknown`, and (b) `verify_hash_chain` still flags the row as corrupt
/// (re-serialized `"Unknown"` != stored garbage), preserving integrity
/// semantics.
#[test]
fn unrecognized_kind_routes_to_unknown_and_keeps_chain_corrupt() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    journal.append_event(
        JournalEventKind::RunCompleted,
        None,
        None,
        Some("evt_1"),
        json!({}),
    )?;
    journal.tamper_first_event_kind_for_test("GarbageKind")?;

    let events = journal.events()?;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind,
        JournalEventKind::Unknown,
        "unrecognized kind must route to Unknown, not RunCompleted"
    );
    assert!(
        !journal.verify_hash_chain()?,
        "hash chain must remain flagged corrupt when a kind was tampered"
    );
    Ok(())
}

/// The core behavioral fix: an unrecognized kind must NOT be treated as a
/// delivered-set kind by `undelivered_ingress_events`. Before the fix,
/// corrupting a SessionReady row's kind to garbage still left the correlated
/// ingress "delivered" because the garbage parsed to RunCompleted.
#[test]
fn unknown_kind_is_not_treated_as_delivered() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Append SessionReady FIRST so it is sequence 1 (the row our tamper
    // helper targets), sharing the ingress event_id as correlation_id.
    journal.append_event(
        JournalEventKind::SessionReady,
        None,
        None,
        Some("evt_2"),
        json!({ "session_id": "session_test" }),
    )?;
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some("evt_2"),
        json!({ "event_id": "evt_2" }),
    )?;
    assert!(
        journal.undelivered_ingress_events()?.is_empty(),
        "SessionReady should mark the ingress as delivered before tampering"
    );

    // Corrupt the SessionReady (sequence 1) kind so it is no longer
    // recognized. After the fix it routes to Unknown, which does not match
    // the delivered-set predicate, so the ingress event reappears.
    journal.tamper_first_event_kind_for_test("FutureKind")?;

    let after = journal.undelivered_ingress_events()?;
    assert_eq!(
        after.len(),
        1,
        "unknown kind must not count as delivered; ingress should reappear"
    );
    Ok(())
}
