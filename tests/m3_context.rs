use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::context::ContextAssembler;
use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn context_assembler_loads_files_catalog_recent_and_truncates() -> Result<()> {
    let root = temp_root()?;
    fs::create_dir_all(root.join("system"))?;
    fs::create_dir_all(root.join("agents/main"))?;
    fs::create_dir_all(root.join("skills/chat"))?;
    fs::create_dir_all(root.join("skills/code"))?;
    fs::write(
        root.join("system/root.md"),
        format!("ROOT {}", "r".repeat(80)),
    )?;
    fs::write(root.join("system/runtime.md"), "RUNTIME")?;
    fs::write(root.join("agents/main/AGENT.md"), "AGENT")?;
    fs::write(
        root.join("skills/chat/SKILL.md"),
        format!("# Chat\nshort chat\n{}", "c".repeat(80)),
    )?;
    fs::write(root.join("skills/code/SKILL.md"), "# Code\nwrite code")?;

    let mut config = test_config(root.clone());
    config.context_max_block_chars = 50;
    config.context_recent_messages = 4;
    let journal = JournalStore::in_memory()?;
    let session = session();
    link_user_message(&journal, &session.id, "event_prior", "previous message")?;
    link_user_message(
        &journal,
        &session.id,
        "event_current",
        "current should be excluded",
    )?;

    let event = validated_event("event_current", "current text");
    let grants: Vec<String> = event
        .principal
        .grants
        .iter()
        .map(|g| g.operation.clone())
        .collect();
    let blocks = ContextAssembler::from_config(&config).build(
        &journal,
        &session,
        &event,
        "current text that is intentionally longer than the tiny budget",
        &grants,
        None,
    )?;

    let root_block = block(&blocks, ContextBlockKind::RootSystem);
    assert!(root_block.content.len() > 50);
    assert!(block(&blocks, ContextBlockKind::SkillCatalog)
        .content
        .contains("code: write code"));
    assert!(block(&blocks, ContextBlockKind::ActiveSkill)
        .content
        .contains("[truncated]"));
    let recent = block(&blocks, ContextBlockKind::RecentMessages);
    assert!(recent.content.contains("previous message"));
    assert!(!recent.content.contains("current should be excluded"));
    assert!(block(&blocks, ContextBlockKind::UserMessage)
        .content
        .contains("[truncated]"));
    fs::remove_dir_all(root)?;
    Ok(())
}

fn link_user_message(
    journal: &JournalStore,
    session_id: &SessionId,
    event_id: &str,
    text: &str,
) -> Result<()> {
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some(&format!("feishu:message:{event_id}")),
        json!({ "source": "feishu", "event_id": event_id, "text": text }),
    )?;
    journal.append_event(
        JournalEventKind::SessionReady,
        None,
        Some(session_id),
        Some(event_id),
        json!({ "session_id": session_id.0 }),
    )?;
    Ok(())
}

fn block(blocks: &[ContextBlock], kind: ContextBlockKind) -> &ContextBlock {
    blocks
        .iter()
        .find(|block| std::mem::discriminant(&block.kind) == std::mem::discriminant(&kind))
        .expect("block exists")
}

fn temp_root() -> Result<PathBuf> {
    let root = std::env::temp_dir().join(format!("agent-core-context-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    Ok(root)
}

fn session() -> Session {
    Session {
        id: SessionId("session_test".to_string()),
        agent_id: AgentId("main".to_string()),
        channel: ChannelKind::Feishu,
        conversation_key: "feishu:open_id:ou_user".to_string(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    }
}

fn validated_event(event_id: &str, text: &str) -> ValidatedEvent {
    ValidatedEvent {
        event_id: EventId(event_id.to_string()),
        source: EventSource::Feishu,
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:ou_user".to_string()),
            subject: PrincipalSubject::FeishuOpenId("ou_user".to_string()),
            source: PrincipalSource::Feishu,
            grants: vec![CapabilityGrant {
                operation: "feishu.send_message".to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("feishu:open_id:ou_user".to_string()),
        },
        session_target: SessionTarget {
            agent_id: AgentId("main".to_string()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:ou_user".to_string(),
        },
        payload: RuntimeEventPayload::UserMessage {
            text: text.to_string(),
            message_id: Some("om_current".to_string()),
            chat_id: Some("oc_chat".to_string()),
        },
        dedupe_key: "feishu:message:om_current".to_string(),
        occurred_at: Utc::now(),
    }
}

fn test_config(root_dir: PathBuf) -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: root_dir.clone(),
        agent_id: AgentId("main".to_string()),
        root_dir,
        kernel_port: 4130,
        connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
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
    }
}
