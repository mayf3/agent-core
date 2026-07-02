//! Recall authoritative-receipt audit + no-grant Runtime tests.
//!
//! These two tests are part of the REQUIRED quartet (each runs exactly one
//! test, `running 1 test`):
//!
//!   - recall_recent_records_authoritative_receipt_without_raw_payload
//!   - recall_recent_without_grant_is_rejected_by_runtime
//!
//! They share the faithful two-round provider stub and marker machinery from
//! `recall_test_support`. They MUST NOT be deleted, renamed, or merged.

use super::recall_test_support::{
    activate_recall_snapshot, assert_strict_keys, feishu_envelope, llm_input_blob, test_config,
    CapturingLlm, NoopLlm,
};
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::Value;

// =========================================================================
// Test 3: authoritative Receipt + Proposed/Approved/Receipt direct linkage.
// =========================================================================

#[test]
fn recall_recent_records_authoritative_receipt_without_raw_payload() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    activate_recall_snapshot(&journal)?;
    let gateway = Gateway::new(config.clone());

    // Seed history in the audit session.
    let seed_env = feishu_envelope(
        "ingress_audit_seed",
        "msg_audit_seed",
        "open_id_audit",
        "chat_audit",
        "seed for receipt audit test",
    );
    let seed_event = gateway.validate_ingress(&journal, serde_json::from_value(seed_env)?)?;
    Runtime::new(config.clone(), NoopLlm).deliver(&journal, &gateway, seed_event)?;

    // Recall run.
    let recall_env = feishu_envelope(
        "ingress_recall_audit",
        "msg_recall_audit",
        "open_id_audit",
        "chat_audit",
        "recall for audit test",
    );
    let recall_event = gateway.validate_ingress(&journal, serde_json::from_value(recall_env)?)?;
    let capturing = CapturingLlm::new("recall_call_audit", "audit final reply");
    let inputs_handle = capturing.inputs_handle();
    let outcome = Runtime::new(config, capturing).deliver(&journal, &gateway, recall_event)?;
    let run_id = outcome.run_id.clone();
    let session_id = outcome.session_id.clone();

    let inputs = inputs_handle.lock().unwrap();
    assert_eq!(
        inputs.len(),
        2,
        "audit test requires exactly 2 provider rounds"
    );
    let round1 = &inputs[0];
    let round2 = inputs
        .get(1)
        .expect("provider round 2 required for audit test");

    // Round 1 produced a real provider tool turn.
    assert!(
        round1.follow_ups.is_empty(),
        "round 1 must have no follow-ups"
    );

    // ---- Direct event counts for this run.
    let events = journal.events()?;
    let tool_calls = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ToolCallIssued
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("operation").and_then(Value::as_str)
                    == Some("session.recall_recent")
        })
        .count();
    assert_eq!(tool_calls, 1, "exactly 1 ToolCallIssued for recall");

    let proposed = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::InvocationProposed
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("operation").and_then(Value::as_str)
                    == Some("session.recall_recent")
        })
        .count();
    assert_eq!(proposed, 1, "exactly 1 InvocationProposed for recall");

    let approved = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::InvocationApproved
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("operation").and_then(Value::as_str)
                    == Some("session.recall_recent")
        })
        .count();
    assert_eq!(approved, 1, "exactly 1 InvocationApproved for recall");

    let receipts = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("invocation_id").is_some()
                && e.payload.get("output").is_some()
        })
        .count();
    assert_eq!(receipts, 1, "exactly 1 ReceiptReceived for recall");

    // ---- Direct field linkage (envelope run_id / session_id).
    let proposed_e = events
        .iter()
        .find(|e| {
            e.kind == JournalEventKind::InvocationProposed && e.run_id.as_ref() == Some(&run_id)
        })
        .expect("InvocationProposed event");
    let approved_e = events
        .iter()
        .find(|e| {
            e.kind == JournalEventKind::InvocationApproved && e.run_id.as_ref() == Some(&run_id)
        })
        .expect("InvocationApproved event");
    let receipt_e = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ReceiptReceived && e.run_id.as_ref() == Some(&run_id))
        .expect("ReceiptReceived event");

    // invocation_id linkage (via correlation_id on Proposed/Approved envelope
    // and payload.invocation_id on Receipt).
    let proposed_invocation = proposed_e.correlation_id.as_ref();
    let approved_invocation = approved_e.correlation_id.as_ref();
    let receipt_invocation = receipt_e
        .payload
        .get("invocation_id")
        .and_then(Value::as_str);
    assert!(
        proposed_invocation.is_some(),
        "InvocationProposed must carry a correlation invocation_id"
    );
    assert_eq!(
        proposed_invocation, approved_invocation,
        "proposed.invocation_id == approved.invocation_id"
    );
    assert_eq!(
        approved_invocation.map(|s| s.to_string()),
        receipt_invocation.map(|s| s.to_string()),
        "approved.invocation_id == receipt.invocation_id"
    );

    // run_id linkage (envelope).
    assert_eq!(proposed_e.run_id, Some(run_id.clone()));
    assert_eq!(approved_e.run_id, Some(run_id.clone()));
    assert_eq!(receipt_e.run_id, Some(run_id.clone()));

    // session_id linkage (envelope).
    assert_eq!(proposed_e.session_id, Some(session_id.clone()));
    assert_eq!(approved_e.session_id, Some(session_id.clone()));
    assert_eq!(receipt_e.session_id, Some(session_id.clone()));

    // Receipt status Succeeded.
    assert_eq!(
        receipt_e.payload.get("status").and_then(Value::as_str),
        Some("Succeeded")
    );

    // ---- tool_call_id consistency across the two rounds.
    let round1_call_id = "recall_call_audit";
    let round2_has_call_id = round2
        .follow_ups
        .iter()
        .any(|fu| fu.provider_turn.provider_tool_call_id == round1_call_id);
    assert!(
        round2_has_call_id,
        "round 2 follow-up must reference round 1 tool_call_id {round1_call_id}"
    );

    // ---- Strict field whitelist + marker scan on Receipt output + ToolResult.
    let receipt_output = receipt_e
        .payload
        .get("output")
        .cloned()
        .expect("receipt output");
    let messages = receipt_output
        .get("messages")
        .and_then(Value::as_array)
        .expect("recall output must contain messages array");
    assert!(
        !messages.is_empty(),
        "audit recall must return non-empty history"
    );
    for item in messages {
        assert_strict_keys(item);
    }
    assert!(
        receipt_output.get("session_id").is_none(),
        "receipt output must not contain session_id"
    );
    let receipt_blob = serde_json::to_string(&receipt_e.payload).unwrap_or_default();
    assert!(
        !receipt_blob.contains("session_id"),
        "receipt payload must not leak session_id: {receipt_blob}"
    );
    for fu in &round2.follow_ups {
        assert!(
            !fu.result_content.contains("session_id"),
            "ToolResult must not contain session_id: {}",
            fu.result_content
        );
    }

    // ReadOnly recall must not queue an outbox for the recall operation.
    let outboxes_recall = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::OutboxQueued
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("operation").and_then(Value::as_str)
                    == Some("session.recall_recent")
        })
        .count();
    assert_eq!(outboxes_recall, 0, "ReadOnly recall must not queue outbox");
    Ok(())
}

// =========================================================================
// Test 4: a Run WITHOUT the session.recall_recent grant must be rejected by
//         the real Runtime::deliver path (policy denies capability_not_enabled).
//
// Because the normal ingress path always grants recall, this test constructs a
// ValidatedEvent whose principal omits only the recall grant and feeds it
// directly through Runtime::deliver. The provider emits a real recall tool
// call; the Gateway policy rejects it; the run continues into round 2 with a
// policy_denied ToolResult. We assert:
//   - zero InvocationApproved / Succeeded ReceiptReceived for recall;
//   - a ToolCallRejected with the policy_denied category;
//   - the bait history never appears in any successful layer.
// =========================================================================

#[test]
fn recall_recent_without_grant_is_rejected_by_runtime() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    activate_recall_snapshot(&journal)?;
    let gateway = Gateway::new(config.clone());

    // ---- 6.1 Seed bait history in the target session.
    let bait_env = feishu_envelope(
        "ingress_bait_nogrant",
        "msg_bait",
        "open_id_nogrant",
        "chat_nogrant",
        "HISTORY_MUST_NOT_BE_RETURNED",
    );
    let bait_event = gateway.validate_ingress(&journal, serde_json::from_value(bait_env)?)?;
    let bait_session_key = "feishu:open_id:open_id_nogrant".to_string();
    Runtime::new(config.clone(), NoopLlm).deliver(&journal, &gateway, bait_event)?;

    // ---- Build a ValidatedEvent whose principal omits the recall grant, then
    //         deliver it through the real Runtime path. The provider mock still
    //         emits a recall tool call, forcing the Runtime to construct an
    //         InvocationIntent and ask the Gateway.
    let recall_envelope = feishu_envelope(
        "ingress_nogrant_recall",
        "msg_nogrant_recall",
        "open_id_nogrant",
        "chat_nogrant",
        "recall without grant",
    );
    // Validate normally to obtain a ValidatedEvent, then strip ONLY the
    // session.recall_recent grant from its principal — keeping the reply grant
    // so the run can still complete its final assistant reply. This proves the
    // absence of the recall grant specifically (capability_not_enabled) while
    // exercising the real Runtime::deliver path.
    let mut recall_event =
        gateway.validate_ingress(&journal, serde_json::from_value(recall_envelope)?)?;
    let original_grants = recall_event.principal.grants.clone();
    assert!(
        original_grants
            .iter()
            .any(|g| g.operation == "session.recall_recent"),
        "baseline ingress must grant session.recall_recent (so stripping it is meaningful)"
    );
    // Remove ONLY the recall grant; keep reply + any other grants intact so the
    // run can finish its final reply (the reply path needs the send grant).
    recall_event
        .principal
        .grants
        .retain(|g| g.operation != "session.recall_recent");
    assert!(
        !recall_event
            .principal
            .grants
            .iter()
            .any(|g| g.operation == "session.recall_recent"),
        "the constructed Run principal must NOT contain the session.recall_recent grant"
    );
    assert!(
        !recall_event.principal.grants.is_empty(),
        "the reply grant must remain so the run can complete"
    );

    // ---- 6.2 Full Runtime::deliver path with a provider that demands recall.
    let capturing = CapturingLlm::new("recall_call_nogrant", "nogrant final reply");
    let inputs_handle = capturing.inputs_handle();
    let outcome = Runtime::new(config, capturing).deliver(&journal, &gateway, recall_event)?;
    let run_id = outcome.run_id.clone();

    let inputs = inputs_handle.lock().unwrap();
    assert_eq!(
        inputs.len(),
        2,
        "no-grant run must still complete round 2 (with a policy_denied tool result)"
    );
    let round2 = inputs
        .get(1)
        .expect("provider round 2 required even after grant denial");

    let events = journal.events()?;

    // ---- Direct assertions: zero approvals / zero successful receipts for
    //         the recall operation in this run.
    let recall_approved = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::InvocationApproved
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("operation").and_then(Value::as_str)
                    == Some("session.recall_recent")
        })
        .count();
    assert_eq!(
        recall_approved, 0,
        "InvocationApproved for recall must be 0 without grant"
    );

    let recall_succeeded_receipts = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("status").and_then(Value::as_str) == Some("Succeeded")
                && e.payload.get("output").is_some()
        })
        .count();
    assert_eq!(
        recall_succeeded_receipts, 0,
        "Succeeded ReceiptReceived for recall must be 0 without grant"
    );

    // ---- The Gateway rejection produced a ToolCallRejected event with the
    //         policy_denied category (the Gateway's capability_not_enabled
    //         denial maps to the Runtime's typed ToolRejection::PolicyDenied).
    let rejection = events
        .iter()
        .find(|e| {
            e.kind == JournalEventKind::ToolCallRejected
                && e.run_id.as_ref() == Some(&run_id)
                && e.payload.get("invocation_id").is_some()
        })
        .expect("ToolCallRejected must exist for the denied recall");
    assert_eq!(
        rejection
            .payload
            .get("error_category")
            .and_then(Value::as_str),
        Some("policy_denied"),
        "the recall tool call must be rejected with the policy_denied category"
    );

    // ---- Bait history must not appear in ANY layer.
    let bait = "HISTORY_MUST_NOT_BE_RETURNED";
    for fu in &round2.follow_ups {
        assert!(
            !fu.result_content.contains(bait),
            "bait history must NOT appear in ToolResult: {}",
            fu.result_content
        );
    }
    let round2_blob = llm_input_blob(round2);
    assert!(
        !round2_blob.contains(bait),
        "bait history must NOT appear in round 2 LlmInput"
    );
    let context_blob = serde_json::to_string(&round2.blocks).unwrap_or_default();
    assert!(
        !context_blob.contains(bait),
        "bait history must NOT appear in Context blocks"
    );
    assert!(
        !outcome.output.contains(bait),
        "bait history must NOT appear in the final reply"
    );
    // No successful receipt at all in this run may carry the bait text.
    let any_receipt_with_bait = events.iter().any(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.run_id.as_ref() == Some(&run_id)
            && serde_json::to_string(&e.payload)
                .unwrap_or_default()
                .contains(bait)
    });
    assert!(
        !any_receipt_with_bait,
        "bait history must NOT appear in any ReceiptReceived of the no-grant run"
    );

    // The run still resolves to the target session (the session that holds the
    // bait) — proving isolation is enforced by policy, not by routing away.
    let target_session = journal.get_or_create_session(&SessionTarget {
        agent_id: AgentId("main".into()),
        channel: ChannelKind::Feishu,
        conversation_key: bait_session_key,
    })?;
    assert_eq!(
        outcome.session_id, target_session.id,
        "no-grant run must execute in the same session that holds the bait"
    );
    Ok(())
}
