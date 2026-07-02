//! Recall isolation, audit, and no-grant Runtime tests.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCallResult};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;


fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"), data_dir: PathBuf::from("."),
        agent_id: AgentId("main".into()), root_dir: PathBuf::from("."),
        kernel_port: 4130, connector_execute_url: String::new(),
        ipc_token: "test".into(),
        feishu_allowed_open_ids: vec![], feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: String::new(), openai_api_key: String::new(),
        model: String::new(), fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(), fallback_model: String::new(),
        model_timeout_ms: 100, context_recent_messages: 6,
        context_max_block_chars: 4000, outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10, extra_allowed_operations: vec![],
        require_write_approval: false, write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false, primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
    }
}

struct NoopLlm;
impl LlmClient for NoopLlm {
    fn complete(&self, _i: LlmInput) -> Result<LlmOutput> {
        Ok(LlmOutput { provider: "t".into(), model: "t".into(), content: "ok".into(),
            journal_payload: json!({}), tool_call: ToolCallResult::Absent, provider_turn: None })
    }
}

// =========================================================================
// Test: No grant — Gateway rejection
// =========================================================================

#[test]
fn recall_recent_without_grant_is_rejected_by_runtime() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Write bait history.
    let bait_env = json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "feishu_bait", "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_nogrant", "sender_type": "user",
            "chat_id": "chat_nogrant", "chat_type": "p2p",
            "message_id": "msg_bait", "message_type": "text",
            "text": "HISTORY_MUST_NOT_BE_RETURNED", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    });
    let bait_event = gateway.validate_ingress(&journal, serde_json::from_value(bait_env)?)?;
    Runtime::new(config.clone(), NoopLlm).deliver(&journal, &gateway, bait_event)?;

    // Create a Run without session.recall_recent grant.
    let session = journal.get_or_create_session(&SessionTarget {
        agent_id: AgentId("main".into()), channel: ChannelKind::Feishu,
        conversation_key: "feishu:open_id:open_id_nogrant".into(),
    })?;
    let run = Run {
        id: RunId::new(), session_id: session.id.clone(),
        agent_id: AgentId("main".into()), trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:nogrant".into()),
            subject: PrincipalSubject::FeishuOpenId("open_id_nogrant".into()),
            source: PrincipalSource::Feishu, grants: vec![], requester_id: None,
        },
        parent_run_id: None, delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(),
        registry_snapshot_id: String::new(),
    };

    let snap = crate::registry::snapshot::test_snapshot();
    let intent = InvocationIntent {
        invocation_id: InvocationId::new(), run_id: run.id.clone(),
        operation: "session.recall_recent".into(),
        arguments: json!({}), idempotency_key: Some("recall:nogrant".into()),
    };
    let result = gateway.approve_invocation(intent, &run, &session, &snap);
    assert!(result.is_err(), "Gateway must reject without grant");
    assert!(format!("{}", result.unwrap_err()).contains("capability_not_enabled"),
        "must be capability_not_enabled");

    Ok(())
}

// =========================================================================
// Test: Receipt + Journal audit chain
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
    Runtime::new(config.clone(), NoopLlm).deliver(&journal, &gateway, event)?;

    // Recall with CapturingRecallLlm.
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
    let capturing_inputs = capturing.inputs.clone();
    let outcome2 = Runtime::new(config, capturing).deliver(&journal, &gateway, event2)?;

    let events = journal.events()?;
    let recall_run_id = &outcome2.run_id;

    // Four events.
    let tool_calls = events.iter().filter(|e| {
        e.kind == JournalEventKind::ToolCallIssued && e.run_id.as_ref() == Some(recall_run_id)
            && e.payload.get("operation").and_then(|v| v.as_str()) == Some("session.recall_recent")
    }).count();
    assert_eq!(tool_calls, 1, "exactly 1 ToolCallIssued for recall");

    let proposed = events.iter().filter(|e| {
        e.kind == JournalEventKind::InvocationProposed && e.run_id.as_ref() == Some(recall_run_id)
            && e.payload.get("operation").and_then(|v| v.as_str()) == Some("session.recall_recent")
    }).count();
    assert_eq!(proposed, 1, "exactly 1 InvocationProposed for recall");

    let approved = events.iter().filter(|e| {
        e.kind == JournalEventKind::InvocationApproved && e.run_id.as_ref() == Some(recall_run_id)
            && e.payload.get("operation").and_then(|v| v.as_str()) == Some("session.recall_recent")
    }).count();
    assert_eq!(approved, 1, "exactly 1 InvocationApproved for recall");

    let receipts = events.iter().filter(|e| {
        e.kind == JournalEventKind::ReceiptReceived && e.run_id.as_ref() == Some(recall_run_id)
            && e.payload.get("invocation_id").is_some()
    }).count();
    assert_eq!(receipts, 1, "exactly 1 ReceiptReceived for recall");

    // Verify all four have same run_id and session_id.
    let issued_e = events.iter().find(|e| {
        e.kind == JournalEventKind::ToolCallIssued && e.run_id.as_ref() == Some(recall_run_id)
    }).unwrap();
    let proposed_e = events.iter().find(|e| {
        e.kind == JournalEventKind::InvocationProposed && e.run_id.as_ref() == Some(recall_run_id)
    }).unwrap();
    let approved_e = events.iter().find(|e| {
        e.kind == JournalEventKind::InvocationApproved && e.run_id.as_ref() == Some(recall_run_id)
    }).unwrap();
    let receipt_e = events.iter().find(|e| {
        e.kind == JournalEventKind::ReceiptReceived && e.run_id.as_ref() == Some(recall_run_id)
    }).unwrap();

    assert_eq!(issued_e.run_id, proposed_e.run_id);
    assert_eq!(proposed_e.run_id, approved_e.run_id);
    assert_eq!(approved_e.run_id, receipt_e.run_id);
    assert_eq!(issued_e.session_id, proposed_e.session_id);
    assert_eq!(proposed_e.session_id, approved_e.session_id);
    assert_eq!(approved_e.session_id, receipt_e.session_id);

    // Receipt status.
    assert_eq!(receipt_e.payload.get("status").and_then(|v| v.as_str()), Some("Succeeded"));

    // Provider round 2 follow_ups must not contain session_id.
    let inputs = capturing_inputs.lock().unwrap();
    if inputs.len() >= 2 {
        for fu in &inputs[1].follow_ups {
            assert!(!fu.result_content.contains("session_id"),
                "ToolResult must not contain session_id");
        }
    }

    // ReadOnly recall must not produce outbox for the recall operation.
    let outboxes_recall = events.iter().filter(|e| {
        e.kind == JournalEventKind::OutboxQueued
            && e.run_id.as_ref() == Some(recall_run_id)
            && e.payload.get("operation").and_then(|v| v.as_str()) == Some("session.recall_recent")
    }).count();
    assert_eq!(outboxes_recall, 0, "ReadOnly recall must not queue outbox");

    Ok(())
}
