use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn journal_finds_accepted_ingress_without_delivery() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let accepted = gateway.validate_ingress(&journal, feishu_envelope("evt_1", "om_1")?)?;

    let undelivered = journal.undelivered_ingress_events()?;

    assert_eq!(undelivered.len(), 1);
    assert_eq!(
        undelivered[0]
            .payload
            .get("event_id")
            .and_then(|value| value.as_str()),
        Some(accepted.event_id.0.as_str())
    );
    journal.append_event(
        JournalEventKind::SessionReady,
        None,
        None,
        Some(&accepted.event_id.0),
        json!({ "session_id": "session_test" }),
    )?;
    assert!(journal.undelivered_ingress_events()?.is_empty());
    Ok(())
}

#[test]
fn gateway_recovers_feishu_event_from_ingress_journal_payload() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let accepted = gateway.validate_ingress(&journal, feishu_envelope("evt_2", "om_2")?)?;
    let journal_event = journal.undelivered_ingress_events()?.remove(0);

    let recovered = gateway.recover_validated_event(&journal_event)?;

    assert_eq!(recovered.event_id, accepted.event_id);
    assert_eq!(recovered.dedupe_key, accepted.dedupe_key);
    assert_eq!(
        recovered.session_target.conversation_key,
        "feishu:open_id:ou_user"
    );
    let RuntimeEventPayload::UserMessage {
        text,
        message_id,
        chat_id,
    } = recovered.payload;
    assert_eq!(text, "你好");
    assert_eq!(message_id.as_deref(), Some("om_2"));
    assert_eq!(chat_id.as_deref(), Some("oc_chat"));
    Ok(())
}

#[test]
fn health_reports_undelivered_ingress_count() -> Result<()> {
    let mut config = test_config();
    config.feishu_allowed_open_ids = vec!["ou_user".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let accepted = gateway.validate_ingress(&journal, feishu_envelope("evt_3", "om_3")?)?;

    assert_eq!(
        health_snapshot(&journal, false, &DispatcherMetrics::new())?
            .get("undelivered_ingress_count")
            .and_then(|value| value.as_u64()),
        Some(1)
    );
    journal.append_event(
        JournalEventKind::RunStarted,
        None,
        None,
        Some(&accepted.event_id.0),
        json!({ "run_id": "run_test" }),
    )?;
    assert_eq!(
        health_snapshot(&journal, false, &DispatcherMetrics::new())?
            .get("undelivered_ingress_count")
            .and_then(|value| value.as_u64()),
        Some(0)
    );
    Ok(())
}

fn feishu_envelope(event_id: &str, message_id: &str) -> Result<IngressEnvelope> {
    Ok(IngressEnvelope {
        protocol_version: "v1".to_string(),
        source: ExternalSource::Feishu,
        external_event_id: event_id.to_string(),
        received_at: Utc::now(),
        payload: json!({
            "sender_open_id": "ou_user",
            "sender_type": "user",
            "chat_id": "oc_chat",
            "chat_type": "p2p",
            "message_id": message_id,
            "message_type": "text",
            "text": "你好",
            "mentions": [],
        }),
        auth_context: AuthContext {
            authenticated: true,
        },
        routing_hint: None,
    })
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
    }
}
