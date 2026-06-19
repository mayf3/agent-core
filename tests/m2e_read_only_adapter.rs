//! Phase 2 M2e + PR1: read-only tool tests (time.now + session.recall_recent).

mod common;

use agent_core_kernel::adapters::InvocationAdapter;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmClient, LlmInput, LlmOutput, ToolCall};
use agent_core_kernel::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::sync::Mutex;

fn run_with_time_grant(session_id: &SessionId) -> Run {
    Run {
        id: RunId::new(),
        session_id: session_id.clone(),
        agent_id: AgentId("main".to_string()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:local".to_string()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![CapabilityGrant {
                operation: "time.now".to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("cli:local".to_string()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn time_now_is_catalogued_as_read_only() {
    let spec = agent_core_kernel::domain::operation::lookup("time.now").unwrap();
    assert_eq!(spec.risk, agent_core_kernel::domain::operation::Risk::ReadOnly);
}

#[test]
fn time_now_walks_intent_policy_adapter_receipt() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let session = common::test_session(&config);
    let run = run_with_time_grant(&session.id);
    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: "time.now".to_string(),
        arguments: json!({ "session_id": session.id.0 }),
        idempotency_key: Some("time:1".to_string()),
    };
    let approved = gateway.approve_invocation(intent, &run, &session)?;
    let receipt = agent_core_kernel::adapters::TimeAdapter.execute(&approved)?;
    assert_eq!(receipt.status, ReceiptStatus::Succeeded);
    assert!(receipt.external_ref.is_none());
    Ok(())
}

#[test]
fn time_now_denied_without_grant() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let session = common::test_session(&config);
    let mut run = run_with_time_grant(&session.id);
    run.principal.grants.clear();
    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: "time.now".to_string(),
        arguments: json!({ "session_id": session.id.0 }),
        idempotency_key: Some("time:2".to_string()),
    };
    let err = gateway.approve_invocation(intent, &run, &session).unwrap_err();
    assert!(err.to_string().contains("capability_not_enabled"));
    Ok(())
}

// ---- session.recall_recent ----

struct RecallLlm { call_count: Mutex<usize> }

impl LlmClient for RecallLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        *self.call_count.lock().unwrap() += 1;
        Ok(LlmOutput {
            provider: "test".into(),
            model: "recall-test".into(),
            content: "Recalling...".into(),
            journal_payload: json!({}),
            tool_call: Some(ToolCall {
                id: "call_1".into(),
                operation: "session.recall_recent".into(),
                arguments: json!({ "limit": 10 }),
            }),
        })
    }
}

fn deliver_recall(text: &str) -> Result<(Vec<JournalEvent>, RunId)> {
    let config = common::test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, RecallLlm { call_count: Mutex::new(0) });
    let env = gateway.cli_ingress(text.to_string())?;
    let event = gateway.validate_ingress(&journal, env)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    Ok((journal.events()?, outcome.run_id))
}

#[test]
fn session_recall_writes_audit_facts() -> Result<()> {
    let (events, run_id) = deliver_recall("hello world")?;
    assert!(events.iter().any(|e| {
        e.run_id.as_ref() == Some(&run_id)
            && e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("output").and_then(|o| o.get("session_id")).is_some()
    }), "receipt must be journaled");
    Ok(())
}

#[test]
fn session_recall_returns_only_normalized_fields() -> Result<()> {
    let (events, _) = deliver_recall("my secret token is abc123")?;
    let receipt = events.iter().find(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("output").and_then(|o| o.get("messages")).is_some()
    }).unwrap();
    let messages = receipt.payload.get("output").unwrap()
        .get("messages").unwrap().as_array().unwrap();
    assert!(!messages.is_empty());
    for msg in messages {
        assert!(msg.get("event_id").is_some());
        assert!(msg.get("role").is_some());
        assert!(msg.get("text").is_some());
        assert!(msg.get("payload_json").is_none(), "no raw payload");
    }
    Ok(())
}

#[test]
fn session_recall_does_not_cross_sessions() -> Result<()> {
    let config = common::test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let env_a = gateway.cli_ingress("session A message".into())?;
    let _ = gateway.validate_ingress(&journal, env_a)?;
    let env_b = gateway.cli_ingress("session B message".into())?;
    let event_b = gateway.validate_ingress(&journal, env_b)?;
    let runtime = Runtime::new(config, RecallLlm { call_count: Mutex::new(0) });
    let _ = runtime.deliver(&journal, &gateway, event_b)?;
    let events = journal.events()?;
    let receipt = events.iter().find(|e| {
        e.kind == JournalEventKind::ReceiptReceived
            && e.payload.get("output").and_then(|o| o.get("messages")).is_some()
    }).unwrap();
    let texts: Vec<&str> = receipt.payload.get("output").unwrap()
        .get("messages").unwrap().as_array().unwrap()
        .iter().filter_map(|m| m.get("text").and_then(|t| t.as_str())).collect();
    assert!(texts.iter().any(|t| t.contains("session B")));
    assert!(!texts.iter().any(|t| t.contains("session A")));
    Ok(())
}

#[test]
fn validate_accepts_session_recall() {
    use agent_core_kernel::gateway::validate_tool_call;
    assert!(validate_tool_call(
        &ToolCall { id: "c1".into(), operation: "session.recall_recent".into(), arguments: json!({ "limit": 5 }) },
        &RunId::new(),
    ).is_ok());
}

#[test]
fn validate_rejects_write_via_tool_call() {
    use agent_core_kernel::gateway::validate_tool_call;
    let err = validate_tool_call(
        &ToolCall { id: "c1".into(), operation: "feishu.send_message".into(), arguments: json!({}) },
        &RunId::new(),
    ).unwrap_err();
    assert!(err.to_string().contains("write_operation_not_allowed"));
}
