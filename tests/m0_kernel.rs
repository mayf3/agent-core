use agent_core_kernel::adapters::StdoutAdapter;
use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::LocalEchoLlm;
use agent_core_kernel::runtime::Runtime;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn m0_cli_vertical_slice_writes_journal_and_receipt() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm, StdoutAdapter);
    let envelope = gateway.cli_ingress("你好".to_string())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    let events = journal.events()?;

    assert_eq!(outcome.output, "收到：你好");
    assert!(journal.verify_hash_chain()?);
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::IngressAccepted));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::ReceiptReceived));
    assert!(events.iter().any(|event| event
        .correlation_id
        .as_deref()
        .unwrap_or("")
        .starts_with("invocation_")));
    Ok(())
}

#[test]
fn gateway_deduplicates_ingress_before_runtime() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let envelope = gateway.cli_ingress("once".to_string())?;

    assert!(gateway.validate_ingress(&journal, envelope.clone()).is_ok());
    assert!(gateway.validate_ingress(&journal, envelope).is_err());
    Ok(())
}

#[test]
fn gateway_rejects_stdout_target_mismatch() -> Result<()> {
    let config = test_config();
    let gateway = Gateway::new(config.clone());
    let session = Session {
        id: SessionId("session_current".to_string()),
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Cli,
        conversation_key: "local".to_string(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id.clone(),
        agent_id: config.agent_id,
        trigger_event_id: EventId::new(),
        principal: cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: "stdout.send_text".to_string(),
        arguments: json!({ "session_id": "session_other", "text": "bad" }),
        idempotency_key: None,
    };

    assert!(gateway.approve_invocation(intent, &run, &session).is_err());
    Ok(())
}

#[test]
fn hash_chain_detects_tampering() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm, StdoutAdapter);
    let event = gateway.validate_ingress(&journal, gateway.cli_ingress("hash".to_string())?)?;
    runtime.deliver(&journal, &gateway, event)?;

    assert!(journal.verify_hash_chain()?);
    journal.tamper_first_event_for_test()?;
    assert!(!journal.verify_hash_chain()?);
    Ok(())
}

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
    }
}

fn cli_principal() -> RunPrincipal {
    RunPrincipal {
        principal_id: PrincipalId("cli:local".to_string()),
        subject: PrincipalSubject::LocalUser,
        source: PrincipalSource::Cli,
        grants: vec![CapabilityGrant {
            operation: "stdout.send_text".to_string(),
            scope: "current_session".to_string(),
        }],
        requester_id: Some("cli:local".to_string()),
    }
}
