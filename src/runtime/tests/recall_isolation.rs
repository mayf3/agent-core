//! Recall isolation tests: non-empty field-whitelisted output + cross-session
//! isolation.
//!
//! These two tests are part of the REQUIRED quartet (each runs exactly one
//! test, `running 1 test`):
//!
//!   - recall_recent_non_empty_output_is_field_whitelisted
//!   - recall_recent_isolated_between_distinct_sessions
//!
//! They share the faithful two-round provider stub and marker machinery from
//! `recall_test_support`. They MUST NOT be deleted, renamed, or merged.

use super::recall_test_support::{
    activate_recall_snapshot, assert_strict_keys, feishu_envelope, llm_input_blob, test_config,
    CapturingLlm, NoopLlm, SENSITIVE_MARKERS,
};
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::{json, Value};

// =========================================================================
// Test 1: non-empty recall output is field-whitelisted, with full marker
//         leakage scan across all derived layers.
// =========================================================================

#[test]
fn recall_recent_non_empty_output_is_field_whitelisted() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    activate_recall_snapshot(&journal)?;
    let gateway = Gateway::new(config.clone());

    // ---- 3.1 Seed a real ingress event whose authoritative payload carries
    //         BOTH the visible text and a full set of sensitive markers in
    //         sibling fields. `normalized_text` reads only `payload.text`, so
    //         the marker values live in sibling keys — present in the source
    //         of truth, but never returned by Recall.
    let text_with_marker = "VISIBLE_HISTORY_TEXT";
    let mut seed_env = feishu_envelope(
        "ingress_seed_whitelist",
        "message-id-secret-marker",
        "open_id_whitelist",
        "chat-id-secret-marker",
        text_with_marker,
    );
    if let Some(p) = seed_env.get_mut("payload").and_then(|p| p.as_object_mut()) {
        p.insert(
            "authorization".into(),
            json!("Bearer SECRET_AUTHORIZATION_VALUE"),
        );
        p.insert(
            "private_connector_field".into(),
            json!("PRIVATE_CONNECTOR_FIELD"),
        );
        p.insert("internal_path".into(), json!("/private/internal/path"));
        p.insert(
            "raw_connector_payload".into(),
            json!("RAW_CONNECTOR_PAYLOAD_MARKER"),
        );
        p.insert(
            "nested".into(),
            json!({ "secret": "SECRET_RECALL_PAYLOAD_MARKER" }),
        );
    }
    let seed_event = gateway.validate_ingress(&journal, serde_json::from_value(seed_env)?)?;
    Runtime::new(config.clone(), NoopLlm).deliver(&journal, &gateway, seed_event)?;

    // ---- Directly read the source-of-truth IngressAccepted event and assert
    //         EVERY marker is genuinely present. If any marker was not written,
    //         this test MUST fail.
    let events = journal.events()?;
    let source_ingress = events
        .iter()
        .find(|e| e.kind == JournalEventKind::IngressAccepted)
        .expect("IngressAccepted event must exist after ingress");
    assert_eq!(
        source_ingress.payload.get("text").and_then(Value::as_str),
        Some(text_with_marker),
        "source ingress text must be VISIBLE_HISTORY_TEXT"
    );
    assert_eq!(
        source_ingress
            .payload
            .get("message_id")
            .and_then(Value::as_str),
        Some("message-id-secret-marker"),
        "source ingress must carry message-id marker"
    );
    assert_eq!(
        source_ingress
            .payload
            .get("chat_id")
            .and_then(Value::as_str),
        Some("chat-id-secret-marker"),
        "source ingress must carry chat-id marker"
    );
    let source_blob = serde_json::to_string(&source_ingress.payload).unwrap_or_default();
    for marker in [
        "message-id-secret-marker",
        "chat-id-secret-marker",
        text_with_marker,
    ] {
        assert!(
            source_blob.contains(marker),
            "source-of-truth IngressAccepted must contain {marker:?}"
        );
    }

    // ---- 3.2 Full Runtime::deliver two-round run with CapturingLlm.
    let recall_env = feishu_envelope(
        "ingress_recall_whitelist",
        "msg_recall_whitelist",
        "open_id_whitelist",
        "chat-id-secret-marker",
        "please recall recent",
    );
    let recall_event = gateway.validate_ingress(&journal, serde_json::from_value(recall_env)?)?;
    let capturing = CapturingLlm::new("recall_call_wl", "final reply containing recalled data");
    let inputs_handle = capturing.inputs_handle();
    let outcome = Runtime::new(config, capturing).deliver(&journal, &gateway, recall_event)?;
    let run_id = outcome.run_id.clone();

    // ---- Two complete provider rounds were captured.
    let inputs = inputs_handle.lock().unwrap();
    assert_eq!(
        inputs.len(),
        2,
        "Provider must be invoked exactly twice (round 1 tool call, round 2 final)"
    );
    let round1 = &inputs[0];
    assert!(
        round1.follow_ups.is_empty(),
        "round 1 must have no follow-ups"
    );
    // Round 2 must carry a non-empty ToolResult follow-up.
    let round2 = inputs.get(1).expect("provider round 2 required");
    assert!(
        !round2.follow_ups.is_empty(),
        "round 2 must have a non-empty role=tool result follow-up"
    );

    // ---- Locate the authoritative Recall ReceiptReceived for this run.
    let events = journal.events()?;
    let receipt = events
        .iter()
        .find(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("output").is_some()
        })
        .expect("ReceiptReceived with output must exist for the recall run");
    let receipt_output = receipt
        .payload
        .get("output")
        .cloned()
        .expect("ReceiptReceived payload must have output");

    // ---- 3.3 Forcible read of the messages array (NO conditional wrap).
    let messages = receipt_output
        .get("messages")
        .and_then(Value::as_array)
        .expect("recall output must contain a messages array");
    assert!(
        !messages.is_empty(),
        "recall output requires at least one real history item; got empty array"
    );
    // At least one item must carry the visible text.
    assert!(
        messages
            .iter()
            .any(|m| m.get("text").and_then(Value::as_str) == Some(text_with_marker)),
        "at least one recalled item must equal VISIBLE_HISTORY_TEXT"
    );
    // ---- 3.4 Strict key set for every recalled item.
    for item in messages {
        assert_strict_keys(item);
    }
    // The top-level output must not expose an internal session_id.
    assert!(
        receipt_output.get("session_id").is_none(),
        "recall output top-level must not contain session_id: {:?}",
        receipt_output
    );

    // ---- tool_call_id consistency across both rounds.
    let round1_call_id = "recall_call_wl";
    let round2_has_call_id = round2
        .follow_ups
        .iter()
        .any(|fu| fu.provider_turn.provider_tool_call_id == round1_call_id);
    assert!(
        round2_has_call_id,
        "round 2 follow-up must reference round 1 tool_call_id {round1_call_id}"
    );

    // ---- 3.5 Five-layer leakage scan: every layer must contain the visible
    //         text and must NOT contain any sensitive marker.
    let receipt_blob = serde_json::to_string(&receipt.payload).unwrap_or_default();
    assert!(
        receipt_blob.contains(text_with_marker),
        "ReceiptReceived must contain visible text: {receipt_blob}"
    );
    for m in SENSITIVE_MARKERS {
        assert!(
            !receipt_blob.contains(m),
            "ReceiptReceived must NOT contain sensitive marker {m:?}: {receipt_blob}"
        );
    }

    // (2) Runtime-generated ToolResult (follow-up result_content).
    for fu in &round2.follow_ups {
        assert!(
            fu.result_content.contains(text_with_marker),
            "ToolResult must contain visible text: {}",
            fu.result_content
        );
        for m in SENSITIVE_MARKERS {
            assert!(
                !fu.result_content.contains(m),
                "ToolResult must NOT contain sensitive marker {m:?}: {}",
                fu.result_content
            );
        }
    }

    // (3) Provider round 2 complete LlmInput.
    let round2_blob = llm_input_blob(round2);
    assert!(
        round2_blob.contains(text_with_marker),
        "Provider round 2 must contain visible text"
    );
    for m in SENSITIVE_MARKERS {
        assert!(
            !round2_blob.contains(m),
            "Provider round 2 must NOT contain sensitive marker {m:?}"
        );
    }

    // (4) Context blocks (round 2 input blocks).
    let context_blob = serde_json::to_string(&round2.blocks).unwrap_or_default();
    assert!(
        context_blob.contains(text_with_marker),
        "Context blocks must contain visible text"
    );
    for m in SENSITIVE_MARKERS {
        assert!(
            !context_blob.contains(m),
            "Context blocks must NOT contain sensitive marker {m:?}"
        );
    }

    // (5) Final Assistant reply.
    assert!(
        outcome.output.contains(text_with_marker),
        "final Assistant reply must contain visible text: {}",
        outcome.output
    );
    for m in SENSITIVE_MARKERS {
        assert!(
            !outcome.output.contains(m),
            "final Assistant reply must NOT contain sensitive marker {m:?}: {}",
            outcome.output
        );
    }

    // (6) The recall-derived ReceiptReceived event payload is the authoritative
    //     derived event — assert exactly one and no session_id leak (covered
    //     above by receipt_blob).
    let recall_receipts: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("output").is_some()
        })
        .collect();
    assert_eq!(
        recall_receipts.len(),
        1,
        "exactly one recall-derived ReceiptReceived expected"
    );
    Ok(())
}

// =========================================================================
// Test 2: recall is isolated between two distinct sessions.
// =========================================================================

#[test]
fn recall_recent_isolated_between_distinct_sessions() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    activate_recall_snapshot(&journal)?;
    let gateway = Gateway::new(config.clone());

    // ---- 4.1 Two genuinely distinct sessions via different conversation
    //         identities (different sender_open_id → different key).
    let env_a = feishu_envelope(
        "ingress_iso_a",
        "msg_iso_a",
        "open_id_session_a",
        "chat_iso_a",
        "A_PRIVATE_HISTORY",
    );
    let event_a = gateway.validate_ingress(&journal, serde_json::from_value(env_a)?)?;
    Runtime::new(config.clone(), NoopLlm).deliver(&journal, &gateway, event_a)?;

    let env_b = feishu_envelope(
        "ingress_iso_b",
        "msg_iso_b",
        "open_id_session_b",
        "chat_iso_b",
        "B_VISIBLE_HISTORY",
    );
    let event_b = gateway.validate_ingress(&journal, serde_json::from_value(env_b)?)?;
    Runtime::new(config.clone(), NoopLlm).deliver(&journal, &gateway, event_b)?;

    // Resolve the two real session ids (composite key differs).
    let session_a = journal.get_or_create_session(&SessionTarget {
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Feishu,
        conversation_key: "feishu:open_id:open_id_session_a".into(),
    })?;
    let session_b = journal.get_or_create_session(&SessionTarget {
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Feishu,
        conversation_key: "feishu:open_id:open_id_session_b".into(),
    })?;
    assert_ne!(
        session_a.id, session_b.id,
        "the two sessions must be genuinely distinct"
    );

    // ---- 4.2 Recall inside Session B via full Runtime::deliver.
    let recall_env = feishu_envelope(
        "ingress_iso_recall_b",
        "msg_iso_recall_b",
        "open_id_session_b",
        "chat_iso_b",
        "recall in session b",
    );
    let recall_event = gateway.validate_ingress(&journal, serde_json::from_value(recall_env)?)?;
    let capturing = CapturingLlm::new("recall_call_iso", "iso final reply with b history");
    let inputs_handle = capturing.inputs_handle();
    let outcome = Runtime::new(config, capturing).deliver(&journal, &gateway, recall_event)?;
    let run_id = outcome.run_id.clone();

    let inputs = inputs_handle.lock().unwrap();
    assert_eq!(inputs.len(), 2, "isolation test requires 2 provider rounds");
    let round2 = inputs
        .get(1)
        .expect("provider round 2 required for isolation test");

    // ---- Receipt for Session B recall.
    let events = journal.events()?;
    let receipt = events
        .iter()
        .find(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("output").is_some()
        })
        .expect("ReceiptReceived for session B recall");
    let receipt_output = receipt
        .payload
        .get("output")
        .cloned()
        .expect("receipt output");

    // Forcible messages read.
    let messages = receipt_output
        .get("messages")
        .and_then(Value::as_array)
        .expect("recall output must contain messages array");
    assert!(
        !messages.is_empty(),
        "session B recall must return non-empty history"
    );

    // Every layer must contain B_VISIBLE_HISTORY and NOT A_PRIVATE_HISTORY.
    let receipt_blob = serde_json::to_string(&receipt.payload).unwrap_or_default();
    assert!(
        receipt_blob.contains("B_VISIBLE_HISTORY"),
        "Receipt must contain B_VISIBLE_HISTORY: {receipt_blob}"
    );
    assert!(
        !receipt_blob.contains("A_PRIVATE_HISTORY"),
        "Receipt must NOT contain A_PRIVATE_HISTORY: {receipt_blob}"
    );
    for fu in &round2.follow_ups {
        assert!(
            fu.result_content.contains("B_VISIBLE_HISTORY"),
            "ToolResult must contain B_VISIBLE_HISTORY"
        );
        assert!(
            !fu.result_content.contains("A_PRIVATE_HISTORY"),
            "ToolResult must NOT contain A_PRIVATE_HISTORY"
        );
    }
    assert!(
        outcome.output.contains("B_VISIBLE_HISTORY"),
        "final reply must reference B history"
    );
    assert!(
        !outcome.output.contains("A_PRIVATE_HISTORY"),
        "final reply must NOT reference A history"
    );

    // ---- 4.3 event_id look-back: each recalled item's source event must
    //         belong to Session B, never Session A.
    let ingress_text_by_event: std::collections::HashMap<String, String> = {
        let mut m = std::collections::HashMap::new();
        for e in &events {
            if e.kind != JournalEventKind::IngressAccepted {
                continue;
            }
            if let (Some(id), Some(text)) = (
                e.payload.get("event_id").and_then(Value::as_str),
                e.payload.get("text").and_then(Value::as_str),
            ) {
                m.insert(id.to_string(), text.to_string());
            }
        }
        m
    };
    for item in messages {
        let event_id = item
            .get("event_id")
            .and_then(Value::as_str)
            .expect("recalled item must have event_id");
        // Find the SessionReady row whose correlation_id == event_id and read
        // its session_id from the envelope.
        let owning_session = events.iter().find_map(|e| {
            if e.kind == JournalEventKind::SessionReady
                && e.correlation_id.as_deref() == Some(event_id)
            {
                e.session_id.clone()
            } else {
                None
            }
        });
        let owning_session =
            owning_session.expect("every recalled event_id must have a SessionReady owner");
        assert_eq!(
            owning_session, session_b.id,
            "recalled event_id {event_id} must belong to session B"
        );
        assert_ne!(
            owning_session, session_a.id,
            "recalled event_id {event_id} must NOT belong to session A"
        );
        // The recalled text must match the source ingress text.
        let src_text = ingress_text_by_event
            .get(event_id)
            .unwrap_or_else(|| panic!("source ingress text for {event_id} must exist"));
        assert_eq!(
            item.get("text").and_then(Value::as_str),
            Some(src_text.as_str()),
            "recalled text must equal source ingress text"
        );
    }
    Ok(())
}
