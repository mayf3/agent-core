//! Recall isolation, receipt audit, and no-grant rejection tests.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;

// =========================================================================
// AssertRecallLlm — asserts specific condition, used in audit test
// =========================================================================

#[allow(dead_code)]
struct RecallLlm;
impl LlmClient for RecallLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        Ok(LlmOutput {
            provider: "test".into(),
            model: "test".into(),
            content: String::new(),
            journal_payload: json!({}),
            tool_call: ToolCallResult::Valid(ToolCall {
                id: "recall_audit_call".into(),
                operation: "session.recall_recent".into(),
                arguments: json!({}),
            }),
            provider_turn: None,
        })
    }
}

struct NoopLlm;
impl LlmClient for NoopLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        Ok(LlmOutput {
            provider: "test".into(),
            model: "test".into(),
            content: "done".into(),
            journal_payload: json!({}),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from("."),
        agent_id: AgentId("main".into()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: String::new(),
        ipc_token: "test".into(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: String::new(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
    }
}

// =========================================================================
// Test 2: Session isolation
// =========================================================================

#[test]
fn recall_recent_isolated_between_distinct_sessions() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Session A: Feishu open_id_a.
    let env_a = json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "feishu_session_a",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_a", "sender_type": "user",
            "chat_id": "chat_a", "chat_type": "p2p",
            "message_id": "msg_a_001", "message_type": "text",
            "text": "A_PRIVATE_HISTORY_session_A_only", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    });
    let event_a = gateway.validate_ingress(&journal, serde_json::from_value(env_a)?)?;
    let event_a_id = event_a.event_id.0.clone();
    let session_a = journal.get_or_create_session(&event_a.session_target)?;
    let runtime_a = Runtime::new(config.clone(), NoopLlm);
    runtime_a.deliver(&journal, &gateway, event_a)?;

    // Write IngressAccepted for recall function.
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        Some(&session_a.id),
        None,
        json!({"event_id": event_a_id, "text": "A_PRIVATE_HISTORY_session_A_only"}),
    )?;

    // Session B: Feishu open_id_b.
    let env_b = json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "feishu_session_b",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_b", "sender_type": "user",
            "chat_id": "chat_b", "chat_type": "p2p",
            "message_id": "msg_b_001", "message_type": "text",
            "text": "B_VISIBLE_HISTORY_session_B_data", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    });
    let event_b = gateway.validate_ingress(&journal, serde_json::from_value(env_b)?)?;
    let event_b_id = event_b.event_id.0.clone();
    let session_b = journal.get_or_create_session(&event_b.session_target)?;
    let runtime_b = Runtime::new(config.clone(), NoopLlm);
    runtime_b.deliver(&journal, &gateway, event_b)?;

    // Write IngressAccepted for Session B recall function.
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        Some(&session_b.id),
        None,
        json!({"event_id": event_b_id, "text": "B_VISIBLE_HISTORY_session_B_data"}),
    )?;

    // Verify distinct session IDs.
    let sessions = journal
        .events()?
        .into_iter()
        .filter(|e| e.kind == JournalEventKind::SessionReady)
        .collect::<Vec<_>>();
    assert!(sessions.len() >= 2, "need at least 2 session events");
    let session_a_id = sessions[0].session_id.as_ref().unwrap().clone();
    let session_b_id = sessions[1].session_id.as_ref().unwrap().clone();
    assert_ne!(
        session_a_id.0, session_b_id.0,
        "Session A and B must have different IDs: {} vs {}",
        session_a_id.0, session_b_id.0
    );

    // Recall from Session B.
    let env_recall = json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "feishu_recall_b",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_b", "sender_type": "user",
            "chat_id": "chat_b", "chat_type": "p2p",
            "message_id": "msg_recall_b", "message_type": "text",
            "text": "recall in session B", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    });
    let event_recall = gateway.validate_ingress(&journal, serde_json::from_value(env_recall)?)?;

    let capturing = super::recall_security::CapturingRecallLlm::new();
    let inputs_arc = capturing.inputs.clone();
    let runtime_recall = Runtime::new(config, capturing);
    runtime_recall.deliver(&journal, &gateway, event_recall)?;

    assert_eq!(inputs_arc.lock().unwrap().len(), 2, "must have 2 rounds");

    // Receipt.
    let receipt = journal
        .events()?
        .into_iter()
        .find(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.payload
                    .get("output")
                    .and_then(|o| o.get("messages"))
                    .is_some()
        })
        .expect("ReceiptReceived with messages");
    let receipt_str = serde_json::to_string(&receipt.payload).unwrap_or_default();

    // Session B data present.
    assert!(
        receipt_str.contains("B_VISIBLE_HISTORY"),
        "Session B must see its own history"
    );
    // Session A data absent.
    assert!(
        !receipt_str.contains("A_PRIVATE_HISTORY"),
        "Session B must NOT see Session A history"
    );

    // Verify returned events belong to Session B via session check.
    // The event_id in recall output comes from IngressAccepted payload,
    // not the journal event_id. Check event count and session B data instead.
    assert!(
        !receipt.payload["output"]["messages"]
            .as_array()
            .unwrap()
            .is_empty(),
        "Session B recall must return messages"
    );
    Ok(())
}

// =========================================================================
// Test 3: Receipt/Journal audit chain
// =========================================================================

#[test]
fn recall_recent_records_authoritative_receipt_without_raw_payload() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Seed session.
    let env = json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "ingress_audit_seed",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_audit", "sender_type": "user",
            "chat_id": "chat_audit", "chat_type": "p2p",
            "message_id": "msg_audit_seed", "message_type": "text",
            "text": "seed for receipt audit test", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    });
    let event = gateway.validate_ingress(&journal, serde_json::from_value(env)?)?;
    let rs = Runtime::new(config.clone(), NoopLlm);
    let outcome = rs.deliver(&journal, &gateway, event)?;
    let _run_id = outcome.run_id.clone();
    let _session_id = outcome.session_id.clone();

    // Recall call.
    let env2 = json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "ingress_recall_audit",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_audit", "sender_type": "user",
            "chat_id": "chat_audit", "chat_type": "p2p",
            "message_id": "msg_recall_audit", "message_type": "text",
            "text": "recall for audit test", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    });
    let event2 = gateway.validate_ingress(&journal, serde_json::from_value(env2)?)?;
    let capturing = super::recall_security::CapturingRecallLlm::new();
    let rr = Runtime::new(config, capturing);
    let outcome2 = rr.deliver(&journal, &gateway, event2)?;

    let events = journal.events()?;

    // Find proposed/approved/receipt for session.recall_recent.
    let recall_run_id = &outcome2.run_id;

    let proposed: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::InvocationProposed
                && e.payload.get("operation").and_then(|v| v.as_str())
                    == Some("session.recall_recent")
                && e.run_id.as_ref() == Some(recall_run_id)
        })
        .collect();
    assert_eq!(proposed.len(), 1, "must have 1 proposed for recall run");

    let approved: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::InvocationApproved
                && e.payload.get("operation").and_then(|v| v.as_str())
                    == Some("session.recall_recent")
                && e.run_id.as_ref() == Some(recall_run_id)
        })
        .collect();
    assert_eq!(approved.len(), 1, "must have 1 approved for recall run");

    let receipt_recall: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.run_id.as_ref() == Some(&outcome2.run_id)
        })
        .collect();
    assert_eq!(receipt_recall.len(), 1, "1 receipt for recall run");

    // Find the recall receipt (look for invocation_id).
    let receipt = receipt_recall[0].clone();

    // Proposed/Approved/Receipt are linked via correlation_id (= invocation_id).
    let proposed_corr = proposed[0].correlation_id.as_deref().unwrap_or("");
    let approved_corr = approved[0].correlation_id.as_deref().unwrap_or("");
    let receipt_corr = receipt.correlation_id.as_deref().unwrap_or("");
    let receipt_inv = receipt
        .payload
        .get("invocation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    assert_eq!(
        proposed_corr, approved_corr,
        "proposed/approved correlation_id must match"
    );
    assert_eq!(
        approved_corr, receipt_corr,
        "approved/receipt correlation_id must match"
    );
    assert_eq!(
        receipt_corr, receipt_inv,
        "receipt correlation_id must match payload invocation_id"
    );

    // Run/session association.
    let recall_run_events: Vec<_> = events
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&outcome2.run_id))
        .collect();
    assert!(recall_run_events.len() >= 3, "recall run must have events");

    // Receipt status.
    assert_eq!(
        receipt.payload.get("status").and_then(|v| v.as_str()),
        Some("Succeeded")
    );

    // Receipt output: if non-empty, strict whitelist.
    if let Some(messages) = receipt.payload["output"]["messages"].as_array() {
        if !messages.is_empty() {
            for msg in messages {
                let keys: std::collections::BTreeSet<&str> = msg
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(|k| k.as_str())
                    .collect();
                assert_eq!(
                    keys,
                    std::collections::BTreeSet::from(["event_id", "role", "text"])
                );
            }
        }
    }

    // Tool call ID association (verified via correlation_id above).
    // Round 2 exists (RecallLlm completes once per round, this test's
    // RecallLlm only does 1 round since it always issues a tool call).
    // Actually RecallLlm only does 1 round (tool call), no round 2 since
    // the LLM doesn't respond with text after receiving the tool result.
    // The Runtime handles the tool result and delivers the final reply.
    // For full 2-round capture, use CapturingRecallLlm from recall_security.

    Ok(())
}

// =========================================================================
// Test 4: No grant — Runtime rejection
// =========================================================================

#[test]
fn recall_recent_without_grant_is_rejected_by_runtime() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Write bait history.
    let bait_env = json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "feishu_bait",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_no_grant", "sender_type": "user",
            "chat_id": "chat_no_grant", "chat_type": "p2p",
            "message_id": "msg_bait", "message_type": "text",
            "text": "HISTORY_MUST_NOT_BE_RETURNED", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    });
    let bait_event = gateway.validate_ingress(&journal, serde_json::from_value(bait_env)?)?;
    let rb = Runtime::new(config.clone(), NoopLlm);
    rb.deliver(&journal, &gateway, bait_event)?;

    // Create a run WITHOUT session.recall_recent grant.
    let session = journal.get_or_create_session(&SessionTarget {
        agent_id: AgentId("main".into()),
        channel: ChannelKind::Feishu,
        conversation_key: "open_id_no_grant".into(),
    })?;
    let run = Run {
        id: RunId::new(),
        session_id: session.id.clone(),
        agent_id: AgentId("main".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id_no_grant".into()),
            subject: PrincipalSubject::FeishuOpenId("open_id_no_grant".into()),
            source: PrincipalSource::Feishu,
            grants: vec![], // NO session.recall_recent grant
            requester_id: None,
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: String::new(),
    };

    // Directly test Gateway approval for session.recall_recent without grant.
    let snap = crate::registry::snapshot::test_snapshot();
    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: "session.recall_recent".into(),
        arguments: json!({}),
        idempotency_key: Some("recall:no_grant".into()),
    };
    let result = gateway.approve_invocation(intent, &run, &session, &snap);
    assert!(result.is_err(), "Gateway must reject without grant");
    let err_str = format!("{}", result.unwrap_err());
    assert!(
        err_str.contains("capability_not_enabled"),
        "error must be capability_not_enabled: {err_str}"
    );

    Ok(())
}
