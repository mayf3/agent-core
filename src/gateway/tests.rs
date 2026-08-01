use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::PathBuf;

fn test_config(data_dir: PathBuf) -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir,
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
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec![],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
        budget_hook: crate::hook::HookConfig::default(),
    }
}

fn temp_data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agent-core-gateway-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_bindings(data_dir: &PathBuf, contents: &str) {
    let bindings_dir = data_dir.join("bindings");
    std::fs::create_dir_all(&bindings_dir).unwrap();
    std::fs::write(bindings_dir.join("feishu.json"), contents).unwrap();
}

fn feishu_envelope(
    external_event_id: &str,
    message_id: &str,
    chat_type: &str,
    chat_id: &str,
) -> Value {
    json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": external_event_id,
        "received_at": Utc::now().to_rfc3339(),
        "payload": {
            "sender_open_id": "ou_user",
            "sender_type": "user",
            "chat_id": chat_id,
            "chat_type": chat_type,
            "message_id": message_id,
            "message_type": "text",
            "text": "hello",
            "mentions": [{}]
        },
        "auth_context": { "authenticated": true },
        "routing_hint": {}
    })
}

fn validate(
    gateway: &Gateway,
    journal: &JournalStore,
    envelope: Value,
) -> Result<ValidatedEvent> {
    gateway.validate_ingress(journal, serde_json::from_value(envelope)?)
}

#[test]
fn group_chat_id_resolves_to_bound_agent() -> Result<()> {
    let data_dir = temp_data_dir();
    write_bindings(
        &data_dir,
        r#"{"version":1,"bindings":[{"chat_id":"oc_group_a","agent_id":"worker-a"}]}"#,
    );
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let event = validate(
        &gateway,
        &journal,
        feishu_envelope("evt_1", "msg_1", "group", "oc_group_a"),
    )?;
    assert_eq!(event.session_target.agent_id, AgentId("worker-a".into()));
    Ok(())
}

#[test]
fn unknown_group_chat_id_falls_back_to_default_agent() -> Result<()> {
    let data_dir = temp_data_dir();
    write_bindings(
        &data_dir,
        r#"{"version":1,"bindings":[{"chat_id":"oc_group_a","agent_id":"worker-a"}]}"#,
    );
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let event = validate(
        &gateway,
        &journal,
        feishu_envelope("evt_2", "msg_2", "group", "oc_unknown"),
    )?;
    assert_eq!(event.session_target.agent_id, AgentId("main".into()));
    Ok(())
}

#[test]
fn missing_bindings_file_falls_back_to_default_agent() -> Result<()> {
    let data_dir = temp_data_dir();
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let event = validate(
        &gateway,
        &journal,
        feishu_envelope("evt_3", "msg_3", "group", "oc_group_a"),
    )?;
    assert_eq!(event.session_target.agent_id, AgentId("main".into()));
    Ok(())
}

#[test]
fn p2p_path_is_unaffected_by_bindings() -> Result<()> {
    let data_dir = temp_data_dir();
    write_bindings(
        &data_dir,
        r#"{"version":1,"bindings":[{"chat_id":"oc_group_a","agent_id":"worker-a"}]}"#,
    );
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let event = validate(
        &gateway,
        &journal,
        feishu_envelope("evt_4", "msg_4", "p2p", "oc_p2p"),
    )?;
    assert_eq!(event.session_target.agent_id, AgentId("main".into()));
    assert_eq!(
        event.session_target.conversation_key,
        "feishu:open_id:ou_user"
    );
    Ok(())
}

#[test]
fn invalid_bindings_file_fails_closed_for_group() -> Result<()> {
    let data_dir = temp_data_dir();
    write_bindings(&data_dir, "{ this is not json");
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let err = validate(
        &gateway,
        &journal,
        feishu_envelope("evt_5", "msg_5", "group", "oc_group_a"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid_feishu_bindings"), "err: {err}");
    Ok(())
}

#[test]
fn invalid_bindings_do_not_affect_p2p() -> Result<()> {
    let data_dir = temp_data_dir();
    write_bindings(&data_dir, "{ this is not json");
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let event = validate(
        &gateway,
        &journal,
        feishu_envelope("evt_6", "msg_6", "p2p", "oc_p2p"),
    )?;
    assert_eq!(event.session_target.agent_id, AgentId("main".into()));
    Ok(())
}

#[test]
fn recover_preserves_bound_agent_id() -> Result<()> {
    let data_dir = temp_data_dir();
    write_bindings(
        &data_dir,
        r#"{"version":1,"bindings":[{"chat_id":"oc_group_a","agent_id":"worker-a"}]}"#,
    );
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let accepted = validate(
        &gateway,
        &journal,
        feishu_envelope("evt_7", "msg_7", "group", "oc_group_a"),
    )?;
    let journal_event = journal.undelivered_ingress_events()?.remove(0);
    let recovered = gateway.recover_validated_event(&journal_event)?;
    assert_eq!(
        recovered.session_target.agent_id,
        accepted.session_target.agent_id
    );
    assert_eq!(recovered.session_target.agent_id, AgentId("worker-a".into()));
    Ok(())
}

#[test]
fn recover_old_payload_without_agent_id_falls_back_to_default() -> Result<()> {
    let data_dir = temp_data_dir();
    let gateway = Gateway::new(test_config(data_dir));
    let journal = JournalStore::in_memory()?;
    let event = validated_event_without_agent_id("evt_8", "msg_8")?;
    journal.accept_ingress_with_worker_job(
        &event,
        json!({
            "source": "feishu",
            "event_id": "evt_8",
            "dedupe_key": event.dedupe_key.clone(),
            "sender_open_id": "ou_user",
            "chat_id": "oc_chat",
            "chat_type": "group",
            "conversation_key": "feishu:chat_id:oc_chat",
            "message_id": "msg_8",
            "text": "hello",
        }),
    )?;
    let journal_event = journal.undelivered_ingress_events()?.remove(0);
    let recovered = gateway.recover_validated_event(&journal_event)?;
    assert_eq!(recovered.session_target.agent_id, AgentId("main".into()));
    Ok(())
}

fn validated_event_without_agent_id(event_id: &str, message_id: &str) -> Result<ValidatedEvent> {
    Ok(ValidatedEvent {
        event_id: EventId(event_id.to_string()),
        source: EventSource::Feishu,
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:ou_user".to_string()),
            subject: PrincipalSubject::FeishuOpenId("ou_user".to_string()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: Some("feishu:open_id:ou_user".to_string()),
        },
        session_target: SessionTarget {
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:chat_id:oc_chat".to_string(),
        },
        payload: RuntimeEventPayload::UserMessage {
            text: "hello".to_string(),
            message_id: Some(message_id.to_string()),
            chat_id: Some("oc_chat".to_string()),
        },
        dedupe_key: format!("feishu:message:{message_id}"),
        occurred_at: Utc::now(),
        chat_type: Some("group".to_string()),
    })
}
