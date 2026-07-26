//! SHADOW_SUPPORT_SMOKE_TESTS — Failure-recovery regression tests.
//!
//! These tests verify journal-level behavior for deployment failure scenarios
//! without requiring live services.

use agent_core_kernel::domain::JournalEventKind;
use agent_core_kernel::journal::{
    event_observe::EventObserveQuery, JournalStore,
};
use serde_json::json;

fn make_journal() -> JournalStore {
    JournalStore::in_memory().expect("fresh journal")
}

#[test]
fn activation_failed_does_not_block_new_proposal() {
    let journal = make_journal();

    // Record a deployment failure event
    journal
        .append_event(
            JournalEventKind::CapabilityChangeActivationFailed,
            None,
            None,
            None,
            json!({
                "decision_id": "dec_fail_1",
                "component_id": "failure-viewer",
                "reason": "SERVICE_EXITED_BEFORE_READY",
            }),
        )
        .expect("append ActivationFailed");

    // Verify the failed event is in the journal
    let events = journal
        .observe_events(&EventObserveQuery {
            after_sequence: None,
            limit: 100,
            event_kind: String::new(),
            run_id: String::new(),
            session_id: String::new(),
            principal_id: String::new(),
        })
        .expect("observe");
    let failed: Vec<_> = events
        .events
        .iter()
        .filter(|e| e.event_kind == "CapabilityChangeActivationFailed")
        .collect();
    assert_eq!(failed.len(), 1, "should have one CapabilityChangeActivationFailed");

    // Record a new proposal event after the failure
    journal
        .append_event(
            JournalEventKind::CapabilityChangeProposed,
            None,
            None,
            None,
            json!({
                "proposal_id": "prop_new",
                "component_id": "failure-viewer",
            }),
        )
        .expect("append new proposal");

    // A new CapabilityChangeProposed should be recordable after a failure
    let events = journal
        .observe_events(&EventObserveQuery {
            after_sequence: None,
            limit: 100,
            event_kind: String::new(),
            run_id: String::new(),
            session_id: String::new(),
            principal_id: String::new(),
        })
        .expect("observe");
    let proposals: Vec<_> = events
        .events
        .iter()
        .filter(|e| e.event_kind == "CapabilityChangeProposed")
        .collect();
    assert_eq!(
        proposals.len(),
        1,
        "should have one CapabilityChangeProposed even after failure"
    );
    assert_eq!(failed[0].event_kind, "CapabilityChangeActivationFailed", "event kind should be ActivationFailed");
    assert_eq!(
        proposals[0].payload["proposal_id"], "prop_new",
        "proposal payload should be intact"
    );
}

#[test]
fn multiple_events_preserve_correct_order_after_failure() {
    let journal = make_journal();

    // Append events in sequence: proposal, activation_failed, proposal
    journal
        .append_event(
            JournalEventKind::CapabilityChangeProposed,
            None,
            None,
            None,
            json!({"proposal_id": "prop_1"}),
        )
        .expect("prop_1");
    journal
        .append_event(
            JournalEventKind::CapabilityChangeActivationFailed,
            None,
            None,
            None,
            json!({"decision_id": "dec_fail"}),
        )
        .expect("fail");
    journal
        .append_event(
            JournalEventKind::CapabilityChangeProposed,
            None,
            None,
            None,
            json!({"proposal_id": "prop_2"}),
        )
        .expect("prop_2");

    // Verify order via sequence
    let events = journal
        .observe_events(&EventObserveQuery {
            after_sequence: None,
            limit: 100,
            event_kind: String::new(),
            run_id: String::new(),
            session_id: String::new(),
            principal_id: String::new(),
        })
        .expect("observe");
    let kinds: Vec<&str> = events
        .events
        .iter()
        .map(|e| e.event_kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        &[
            "CapabilityChangeProposed",
            "CapabilityChangeActivationFailed",
            "CapabilityChangeProposed",
        ],
        "event order should be preserved"
    );
}
