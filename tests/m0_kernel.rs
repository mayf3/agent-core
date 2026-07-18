use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{LlmClient, LlmInput, LocalEchoLlm, OpenAiCompatibleLlm};
use agent_core_kernel::runtime::{run_yield, session_spawn, Runtime};
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;
#[test]
fn m0_cli_vertical_slice_writes_journal_and_receipt() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm);
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
        .any(|event| event.kind == JournalEventKind::OutboxQueued));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::InvocationApproved));
    assert!(events.iter().any(|event| event
        .correlation_id
        .as_deref()
        .unwrap_or("")
        .starts_with("reply:run_")));
    assert!(events
        .iter()
        .all(|event| event.kind != JournalEventKind::ReceiptReceived));
    assert!(events
        .iter()
        .all(|event| event.kind != JournalEventKind::DispatchStarted));
    assert!(events
        .iter()
        .all(|event| event.kind != JournalEventKind::RunCompleted));
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
        registry_snapshot_id: String::new(),
        mode: RunMode::Default,
    };
    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: "stdout.send_text".to_string(),
        arguments: json!({ "session_id": "session_other", "text": "bad" }),
        idempotency_key: None,
    };
    let snap = agent_core_kernel::registry::snapshot::test_snapshot();
    assert!(gateway
        .approve_invocation(intent, &run, &session, &snap)
        .is_err());
    Ok(())
}
#[test]
fn hash_chain_detects_tampering() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm);
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
fn journal_recovery_marks_unknown_invocations() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let run_id = RunId::new();
    let session_id = SessionId("session_recovery".to_string());
    let run = Run {
        id: run_id.clone(),
        session_id: session_id.clone(),
        agent_id: AgentId("main".to_string()),
        trigger_event_id: EventId::new(),
        principal: cli_principal(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: String::new(),
        mode: RunMode::Default,
    };
    journal.insert_run(&run)?;
    journal.append_event(
        JournalEventKind::DispatchStarted,
        Some(&run_id),
        Some(&session_id),
        Some("invocation_recovery"),
        json!({ "operation": "feishu.send_message" }),
    )?;
    assert_eq!(journal.recover_unknown_invocations()?, 1);
    assert!(journal.unknown_invocations()?.is_empty());
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::OutboxDispatchUnknown
            && event.correlation_id.as_deref() == Some("invocation_recovery")
    }));
    assert!(
        journal
            .events()?
            .iter()
            .filter(|event| {
                event.kind == JournalEventKind::ReceiptReceived
                    && event.correlation_id.as_deref() == Some("invocation_recovery")
            })
            .count()
            == 0
    );
    assert!(
        journal
            .events()?
            .iter()
            .filter(|event| event.kind == JournalEventKind::RunCompleted)
            .count()
            == 0
    );
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
    let snapshot = health_snapshot(&journal, false, &DispatcherMetrics::new())?;
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
    let runtime = Runtime::new(config, LocalEchoLlm);
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
fn feishu_reply_invocation_is_deterministic_for_run() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm);
    let event = gateway.validate_ingress(
        &journal,
        feishu_envelope_with_message("evt_1", "om_reply_once", "p2p", true)?,
    )?;
    let outcome = runtime.deliver_echo(&journal, &gateway, event)?;
    let expected_invocation_id = format!("reply:{}", outcome.run_id.0);
    let expected_key = format!("feishu-reply:{}", outcome.run_id.0);
    assert!(journal.events()?.iter().any(|event| {
        event.kind == JournalEventKind::InvocationProposed
            && event.correlation_id.as_deref() == Some(expected_invocation_id.as_str())
            && event
                .payload
                .get("idempotency_key")
                .and_then(|value| value.as_str())
                == Some(expected_key.as_str())
    }));
    Ok(())
}
#[test]
fn feishu_deliver_wraps_llm_output_as_send_message() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm);
    let event = gateway.validate_ingress(
        &journal,
        feishu_envelope_with_message("evt_llm", "om_llm", "p2p", true)?,
    )?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert_eq!(outcome.output, "收到：你好");
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
        granted_operations: vec![],
        provider_tools: vec![],
        follow_ups: vec![],
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
fn zai_model_prefix_is_normalized_for_zai_endpoint() -> Result<()> {
    let llm = OpenAiCompatibleLlm::new(
        "https://api.z.ai/api/paas/v4".to_string(),
        String::new(),
        "zai/glm-5.1".to_string(),
        100,
    );
    let output = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "hello".to_string(),
        granted_operations: vec![],
        provider_tools: vec![],
        follow_ups: vec![],
    })?;
    assert_eq!(output.model, "glm-5.1");
    assert_eq!(
        output
            .journal_payload
            .get("model")
            .and_then(|value| value.as_str()),
        Some("glm-5.1")
    );
    Ok(())
}
#[test]
fn provider_model_prefix_is_preserved_for_generic_endpoint() -> Result<()> {
    let llm = OpenAiCompatibleLlm::new(
        "https://openrouter.ai/api/v1".to_string(),
        String::new(),
        "zai/glm-5.1".to_string(),
        100,
    );
    let output = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "hello".to_string(),
        granted_operations: vec![],
        provider_tools: vec![],
        follow_ups: vec![],
    })?;
    assert_eq!(output.model, "zai/glm-5.1");
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
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_coding_owner_id: None,
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        max_tool_rounds: 12,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: agent_core_kernel::hook::HookConfig::default(),
        external_orchestration: None,
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
