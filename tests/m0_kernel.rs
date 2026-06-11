use agent_core_kernel::adapters::{InvocationAdapter, StdoutAdapter};
use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmClient, LlmInput, LocalEchoLlm, OpenAiCompatibleLlm};
use agent_core_kernel::runtime::{run_yield, session_spawn, Runtime};
use agent_core_kernel::server::health_snapshot;
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

#[test]
fn journal_scans_unknown_invocations() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let correlation_id = "invocation_unknown";
    journal.append_event(
        JournalEventKind::DispatchStarted,
        None,
        None,
        Some(correlation_id),
        json!({ "operation": "feishu.send_message" }),
    )?;

    assert_eq!(journal.unknown_invocations()?.len(), 1);

    journal.append_event(
        JournalEventKind::ReceiptReceived,
        None,
        None,
        Some(correlation_id),
        json!({ "status": "Succeeded" }),
    )?;
    assert!(journal.unknown_invocations()?.is_empty());
    Ok(())
}

#[test]
fn health_snapshot_reports_hash_and_unknowns() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    journal.append_event(
        JournalEventKind::DispatchStarted,
        None,
        None,
        Some("invocation_unknown"),
        json!({ "operation": "feishu.send_message" }),
    )?;

    let snapshot = health_snapshot(&journal)?;

    assert_eq!(
        snapshot.get("ok").and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        snapshot
            .get("unknown_invocation_count")
            .and_then(|value| value.as_u64()),
        Some(1)
    );
    assert_eq!(
        snapshot.get("status").and_then(|value| value.as_str()),
        Some("degraded")
    );
    Ok(())
}

#[test]
fn disabled_phase0_runtime_abis_return_not_enabled() {
    assert!(session_spawn()
        .unwrap_err()
        .to_string()
        .contains("not_enabled"));
    assert!(run_yield().unwrap_err().to_string().contains("not_enabled"));
}

#[test]
fn feishu_echo_creates_send_message_invocation() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm, RecordingAdapter);
    let event = gateway.validate_ingress(&journal, feishu_envelope("evt_1", "p2p", true)?)?;
    let outcome = runtime.deliver_echo(&journal, &gateway, event)?;
    let events = journal.events()?;

    assert_eq!(outcome.output, "收到：你好");
    assert!(events.iter().any(|event| {
        event.kind == JournalEventKind::InvocationApproved
            && event
                .payload
                .get("operation")
                .and_then(|value| value.as_str())
                == Some("feishu.send_message")
    }));
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn feishu_deduplicates_by_message_id_across_event_ids() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);

    assert!(gateway
        .validate_ingress(
            &journal,
            feishu_envelope_with_message("evt_first", "om_same", "p2p", true)?,
        )
        .is_ok());
    let duplicate = gateway.validate_ingress(
        &journal,
        feishu_envelope_with_message("evt_redelivered", "om_same", "p2p", true)?,
    );

    assert!(duplicate.is_err());
    assert!(duplicate
        .err()
        .unwrap()
        .to_string()
        .contains("duplicate_ingress"));
    let accepted = journal
        .events()?
        .into_iter()
        .filter(|event| event.kind == JournalEventKind::IngressAccepted)
        .count();
    assert_eq!(accepted, 1);
    Ok(())
}

#[test]
fn feishu_reply_idempotency_key_uses_message_id() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm, RecordingAdapter);
    let event = gateway.validate_ingress(
        &journal,
        feishu_envelope_with_message("evt_1", "om_reply_once", "p2p", true)?,
    )?;

    runtime.deliver_echo(&journal, &gateway, event)?;

    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::InvocationProposed
            && event
                .payload
                .get("idempotency_key")
                .and_then(|value| value.as_str())
                == Some("feishu:reply:om_reply_once")
    }));
    Ok(())
}

#[test]
fn feishu_deliver_wraps_llm_output_as_send_message() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm, RecordingAdapter);
    let event = gateway.validate_ingress(
        &journal,
        feishu_envelope_with_message("evt_llm", "om_llm", "p2p", true)?,
    )?;

    let outcome = runtime.deliver(&journal, &gateway, event)?;

    assert_eq!(outcome.output, "");
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::InvocationProposed
            && event
                .payload
                .get("operation")
                .and_then(|value| value.as_str())
                == Some("feishu.send_message")
    }));
    Ok(())
}

#[test]
fn openai_compatible_llm_missing_config_returns_friendly_output() -> Result<()> {
    let llm = OpenAiCompatibleLlm::new(
        "https://example.invalid/v1".to_string(),
        String::new(),
        String::new(),
        100,
    );

    let output = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "hello".to_string(),
    })?;

    assert_eq!(output.provider, "openai-compatible");
    assert_eq!(
        output
            .journal_payload
            .get("error_category")
            .and_then(|value| value.as_str()),
        Some("model_config_required")
    );
    assert!(output.content.contains("AGENT_CORE_OPENAI_API_KEY"));
    Ok(())
}

#[test]
fn feishu_group_requires_mention() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_chat_ids = vec!["oc_chat".to_string()];
    config.feishu_require_group_mention = true;
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let result = gateway.validate_ingress(&journal, feishu_envelope("evt_2", "group", false)?);

    assert!(result.is_err());
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("bot_not_mentioned"));
    Ok(())
}

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
        openai_api_key: String::new(),
        model: String::new(),
        model_timeout_ms: 100,
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

fn feishu_envelope(event_id: &str, chat_type: &str, mentioned: bool) -> Result<IngressEnvelope> {
    feishu_envelope_with_message(event_id, "om_msg", chat_type, mentioned)
}

fn feishu_envelope_with_message(
    event_id: &str,
    message_id: &str,
    chat_type: &str,
    mentioned: bool,
) -> Result<IngressEnvelope> {
    Ok(IngressEnvelope {
        protocol_version: "v1".to_string(),
        source: ExternalSource::Feishu,
        external_event_id: event_id.to_string(),
        received_at: Utc::now(),
        payload: json!({
            "sender_open_id": "ou_user",
            "sender_type": "user",
            "chat_id": "oc_chat",
            "chat_type": chat_type,
            "message_id": message_id,
            "message_type": "text",
            "text": "你好",
            "mentions": if mentioned { json!([{ "open_id": "ou_bot" }]) } else { json!([]) },
        }),
        auth_context: AuthContext {
            authenticated: true,
        },
        routing_hint: None,
    })
}

struct RecordingAdapter;

impl InvocationAdapter for RecordingAdapter {
    fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
        Ok(Receipt {
            invocation_id: invocation.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            external_ref: Some("om_reply".to_string()),
            output: json!({ "message_id": "om_reply" }),
            occurred_at: Utc::now(),
        })
    }
}
