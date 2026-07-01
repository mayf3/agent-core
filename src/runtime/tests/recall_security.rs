//! Recall security and safety boundary tests.
//!
//! Verifies:
//! 1. Non-empty recall output only contains whitelisted fields (event_id, role, text)
//! 2. Recall between distinct sessions is isolated
//! 3. Recall records authoritative Receipt without raw payload
//! 4. Recall without grant is rejected by the Gateway

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use crate::registry::snapshot::test_snapshot;
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Mutex;

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

/// LLM that always calls session.recall_recent on first call, noop after.
struct RecallThenNoopLlm {
    call_count: Mutex<usize>,
}

impl LlmClient for RecallThenNoopLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        let mut count = self.call_count.lock().unwrap();
        *count += 1;
        if *count == 1 {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "test".into(),
                content: String::new(),
                journal_payload: json!({}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "recall_call_1".into(),
                    operation: "session.recall_recent".into(),
                    arguments: json!({}),
                }),
                provider_turn: None,
            })
        } else {
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
}

/// LLM that never calls tools.
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

const SECRET_MARKER: &str = "A_PRIVATE_HISTORY_SECRET_MARKER_789xyz";
const PRIVATE_CONNECTOR_FIELD: &str = "PRIVATE_CONNECTOR_RAW_DATA";
const INTERNAL_PATH: &str = "/private/internal/path";

// =========================================================================
// §1: Non-empty output field whitelist
// =========================================================================

#[test]
fn recall_recent_non_empty_output_is_field_whitelisted() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Ingress a Feishu message with secret markers in connector-only fields.
    let feishu_payload = json!({
        "sender_open_id": "open_id_wl",
        "sender_type": "user",
        "chat_id": "chat_secret",
        "chat_type": "p2p",
        "message_id": "msg_private_001",
        "message_type": "text",
        "text": "what time is it",
        "mentions": [],
        PRIVATE_CONNECTOR_FIELD: "sensitive_data",
        INTERNAL_PATH: "leaked_path",
    });
    let envelope = serde_json::from_value(json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": "ingress_whitelist",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": feishu_payload,
        "auth_context": { "authenticated": true },
        "routing_hint": {},
    }))?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let runtime = Runtime::new(config.clone(), NoopLlm);
    runtime.deliver(&journal, &gateway, event)?;

    // Second ingress + recall call.
    let envelope2 = serde_json::from_value(json!({
        "protocol_version": "v1",
        "source": "Cli",
        "external_event_id": "ingress_recall_call",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "text": "recall history" },
        "auth_context": { "authenticated": true, "user_identity": "cli:whitelist" },
    }))?;
    let event2 = gateway.validate_ingress(&journal, envelope2)?;
    let runtime2 = Runtime::new(config, RecallThenNoopLlm { call_count: Mutex::new(0) });
    runtime2.deliver(&journal, &gateway, event2)?;

    let events = journal.events()?;
    let receipt = events.iter().find(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("output").and_then(|o| o.get("messages")).is_some()
    }).expect("recall receipt");

    let messages = receipt.payload["output"]["messages"].as_array()
        .expect("messages array");
    assert!(!messages.is_empty(), "recall must return non-empty history");

    for msg in messages {
        assert!(msg.get("event_id").is_some(), "event_id must be present");
        assert!(msg.get("role").is_some(), "role must be present");
        assert!(msg.get("text").is_some(), "text must be present");

        assert!(msg.get("payload_json").is_none(), "payload_json forbidden");
        assert!(msg.get("message_id").is_none(), "message_id forbidden");
        assert!(msg.get("chat_id").is_none(), "chat_id forbidden");
        assert!(msg.get(PRIVATE_CONNECTOR_FIELD).is_none(), "PRIVATE_CONNECTOR_FIELD forbidden");
        assert!(msg.get("authorization").is_none(), "authorization forbidden");
        assert!(msg.get("correlation_meta").is_none(), "correlation_meta forbidden");
        assert!(msg.get(INTERNAL_PATH).is_none(), "internal path forbidden");

        let text = msg["text"].as_str().unwrap_or("");
        assert!(!text.contains(SECRET_MARKER), "secret marker leaked in text");
    }

    // Scan entire journal for forbidden markers.
    for event in &events {
        let s = serde_json::to_string(&event.payload).unwrap_or_default();
        assert!(!s.contains(SECRET_MARKER), "SECRET_MARKER in event {}", event.sequence);
        assert!(!s.contains(PRIVATE_CONNECTOR_FIELD), "PRIVATE_CONNECTOR_FIELD in event {}", event.sequence);
        assert!(!s.contains(INTERNAL_PATH), "INTERNAL_PATH in event {}", event.sequence);
    }

    Ok(())
}

// =========================================================================
// §2: Real session isolation
// =========================================================================

#[test]
fn recall_recent_isolated_between_distinct_sessions() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Use Feishu channel to get distinct sessions by open_id.
    // Session A: Feishu user open_id_a.
    let feishu_event_a = json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": "feishu_session_a",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": {
            "sender_open_id": "open_id_a",
            "sender_type": "user",
            "chat_id": "chat_a",
            "chat_type": "p2p",
            "message_id": "msg_a_001",
            "message_type": "text",
            "text": "A_PRIVATE_HISTORY_session_A_only",
            "mentions": []
        },
        "auth_context": { "authenticated": true },
        "routing_hint": {},
    });
    let env_a = serde_json::from_value(feishu_event_a)?;
    let event_a = gateway.validate_ingress(&journal, env_a)?;
    let ra = Runtime::new(config.clone(), NoopLlm);
    ra.deliver(&journal, &gateway, event_a)?;

    // Session B: Feishu user open_id_b.
    let feishu_event_b = json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": "feishu_session_b",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": {
            "sender_open_id": "open_id_b",
            "sender_type": "user",
            "chat_id": "chat_b",
            "chat_type": "p2p",
            "message_id": "msg_b_001",
            "message_type": "text",
            "text": "B_VISIBLE_HISTORY_session_B_data",
            "mentions": []
        },
        "auth_context": { "authenticated": true },
        "routing_hint": {},
    });
    let env_b = serde_json::from_value(feishu_event_b)?;
    let event_b = gateway.validate_ingress(&journal, env_b)?;
    let rb = Runtime::new(config.clone(), NoopLlm);
    rb.deliver(&journal, &gateway, event_b)?;

    // Call recall from Session B (Feishu open_id_b).
    let feishu_recall = json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": "feishu_recall_b",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": {
            "sender_open_id": "open_id_b",
            "sender_type": "user",
            "chat_id": "chat_b",
            "chat_type": "p2p",
            "message_id": "msg_recall_b",
            "message_type": "text",
            "text": "recall in session B",
            "mentions": []
        },
        "auth_context": { "authenticated": true },
        "routing_hint": {},
    });
    let env_recall = serde_json::from_value(feishu_recall)?;
    let event_recall = gateway.validate_ingress(&journal, env_recall)?;
    let rr = Runtime::new(config, RecallThenNoopLlm { call_count: Mutex::new(0) });
    rr.deliver(&journal, &gateway, event_recall)?;

    let events = journal.events()?;
    let receipt = events.iter().find(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("output").and_then(|o| o.get("messages")).is_some()
    }).expect("recall receipt");

    let texts: Vec<&str> = receipt.payload["output"]["messages"]
        .as_array().unwrap().iter()
        .filter_map(|m| m.get("text").and_then(|t| t.as_str()))
        .collect();

    assert!(texts.iter().any(|t| t.contains("B_VISIBLE_HISTORY")),
        "Session B must see its own history: {texts:?}");
    assert!(!texts.iter().any(|t| t.contains("A_PRIVATE_HISTORY")),
        "Session B must NOT see Session A history: {texts:?}");

    Ok(())
}

// =========================================================================
// §3: Receipt and journal audit chain
// =========================================================================

#[test]
fn recall_recent_records_authoritative_receipt_without_raw_payload() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Seed session.
    let env = serde_json::from_value(json!({
        "protocol_version": "v1", "source": "Cli",
        "external_event_id": "ingress_seed",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "text": "seed for receipt test" },
        "auth_context": { "authenticated": true, "user_identity": "cli:receipt" },
    }))?;
    let event = gateway.validate_ingress(&journal, env)?;
    let rs = Runtime::new(config.clone(), NoopLlm);
    rs.deliver(&journal, &gateway, event)?;

    // Recall call.
    let env2 = serde_json::from_value(json!({
        "protocol_version": "v1", "source": "Cli",
        "external_event_id": "ingress_recall_receipt",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "text": "recall for receipt test" },
        "auth_context": { "authenticated": true, "user_identity": "cli:receipt" },
    }))?;
    let event2 = gateway.validate_ingress(&journal, env2)?;
    let rr = Runtime::new(config, RecallThenNoopLlm { call_count: Mutex::new(0) });
    rr.deliver(&journal, &gateway, event2)?;

    let events = journal.events()?;

    let proposed = events.iter().filter(|e| {
        e.kind == JournalEventKind::InvocationProposed
            && e.payload.get("operation").and_then(|v| v.as_str()) == Some("session.recall_recent")
    }).count();
    assert_eq!(proposed, 1, "exactly 1 InvocationProposed");

    let approved = events.iter().filter(|e| {
        e.kind == JournalEventKind::InvocationApproved
            && e.payload.get("operation").and_then(|v| v.as_str()) == Some("session.recall_recent")
    }).count();
    assert_eq!(approved, 1, "exactly 1 InvocationApproved");

    let receipt_count = events.iter().filter(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("invocation_id").is_some()
    }).count();
    assert_eq!(receipt_count, 1, "exactly 1 ReceiptReceived");

    let receipt = events.iter().find(|e| e.kind == JournalEventKind::ReceiptReceived)
        .expect("receipt exists");

    // Receipt.status == Succeeded.
    assert_eq!(
        receipt.payload.get("status").and_then(|v| v.as_str()),
        Some("Succeeded")
    );

    // Output does not contain raw payload markers.
    let output_str = serde_json::to_string(&receipt.payload).unwrap_or_default();
    assert!(!output_str.contains("payload_json"), "payload_json in receipt output");
    assert!(!output_str.contains(SECRET_MARKER), "SECRET_MARKER in receipt output");

    Ok(())
}

// =========================================================================
// §4: Without grant — rejected
// =========================================================================

#[test]
fn recall_recent_without_grant_is_rejected() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Create a session.
    let env = serde_json::from_value(json!({
        "protocol_version": "v1", "source": "Cli",
        "external_event_id": "ingress_no_grant",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "text": "test without recall grant" },
        "auth_context": { "authenticated": true, "user_identity": "cli:nogrant" },
    }))?;
    let event = gateway.validate_ingress(&journal, env)?;
    let session = journal.get_or_create_session(&event.session_target)?;

    // Create Run without session.recall_recent grant.
    let run = Run {
        id: RunId::new(),
        session_id: session.id.clone(),
        agent_id: AgentId("main".into()),
        trigger_event_id: event.event_id.clone(),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:nogrant".into()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![],
            requester_id: None,
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: String::new(),
    };

    let snap = test_snapshot();
    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: "session.recall_recent".into(),
        arguments: json!({}),
        idempotency_key: Some("recall:no_grant".into()),
    };

    let result = gateway.approve_invocation(intent, &run, &session, &snap);
    assert!(result.is_err(), "must reject without grant");
    assert!(format!("{}", result.unwrap_err()).contains("capability_not_enabled"));

    Ok(())
}
