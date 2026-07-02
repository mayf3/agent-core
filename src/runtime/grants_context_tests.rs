#[cfg(test)]
mod grants_context_tests {
    use crate::config::KernelConfig;
    use crate::context::ContextAssembler;
    use crate::domain::operation::{
        catalog_for_context_grants, provider_tools_for_grants, ExecutionProfile,
    };
    use crate::domain::*;
    use crate::journal::JournalStore;
    use std::path::PathBuf;
    // ===== §4: env → profile → principal → ToolCatalog / Provider tools =====
    //
    // The only correct config key is AGENT_CORE_EXTRA_ALLOWED_OPERATIONS.
    // Tests reuse the production `parse_env_list_value` (not a mirror), so
    // the same split/trim/filter logic that `KernelConfig::from_cli` calls
    // is exercised without mutating global environment variables. Downstream
    // with_extra dedups and drops unknown names; Write ops pass the grant
    // check but are hidden by catalog_for_context_grants / provider_tools_for_grants.
    fn tool_set_from_grants(grants: &[String]) -> Vec<String> {
        provider_tools_for_grants(grants)
            .into_iter()
            .filter_map(|t| {
                t.pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
            .collect()
    }
    fn catalog_set_from_grants(grants: &[String]) -> Vec<String> {
        // The catalog text lists "<name> - <desc>" per line after the header.
        catalog_for_context_grants(grants)
            .lines()
            .skip(1)
            .filter_map(|l| l.split(" - ").next().map(str::to_string))
            .collect()
    }
    #[test]
    fn single_time_now_grant_aligns_catalog_and_provider_tools() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value("system.status"))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let tools = tool_set_from_grants(&grants);
        let catalog = catalog_set_from_grants(&grants);
        assert!(tools.contains(&"system.status".to_string()));
        assert!(catalog.contains(&"system.status".to_string()));
        assert_eq!(
            tools, catalog,
            "ToolCatalog set must equal Provider tools set"
        );
    }
    #[test]
    fn multiple_readonly_grants_whitespace_and_duplicates() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value(
                "  system.status ,  system.status ,, system.status  ",
            ))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        // Deduped: system.status appears once.
        let tools = tool_set_from_grants(&grants);
        assert_eq!(
            tools.iter().filter(|t| t == &"system.status").count(),
            1,
            "duplicates deduped"
        );
        // system.status is auto-added by config; present once.
        assert!(tools.contains(&"system.status".to_string()));
        assert!(tools.contains(&"system.status".to_string()));
        assert_eq!(
            tool_set_from_grants(&grants),
            catalog_set_from_grants(&grants)
        );
    }
    #[test]
    fn unknown_operations_do_not_enter_profile_or_tools() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value(
                "shell.exec, system.status, bogus_op",
            ))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let tools = tool_set_from_grants(&grants);
        assert!(!tools.contains(&"shell.exec".to_string()));
        assert!(!tools.contains(&"bogus_op".to_string()));
        assert!(tools.contains(&"system.status".to_string()));
    }
    #[test]
    fn write_operation_granted_but_never_in_tools_or_catalog() {
        // Even if a Write op is in the env, it is granted (lookup passes) but
        // hidden from BOTH Provider tools and the ToolCatalog (ReadOnly-only).
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value(
                "feishu.send_message, system.status",
            ))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        assert!(
            grants.contains(&"feishu.send_message".to_string()),
            "write op IS granted (policy is the boundary)"
        );
        let tools = tool_set_from_grants(&grants);
        let catalog = catalog_set_from_grants(&grants);
        assert!(!tools.contains(&"feishu.send_message".to_string()));
        assert!(!catalog.contains(&"feishu.send_message".to_string()));
        assert!(tools.contains(&"system.status".to_string()));
    }
    #[test]
    fn empty_grants_yield_no_tools_and_no_catalog_entries() {
        let grants: Vec<String> = vec![];
        let catalog = catalog_for_context_grants(&grants);
        assert!(
            catalog.contains("No tools are available"),
            "no-grants catalog is explicit, not a full list"
        );
        assert!(provider_tools_for_grants(&grants).is_empty());
    }
    // ===== Context ToolCatalog aligns with grants (§1) =====
    fn empty_event(_session: &Session) -> ValidatedEvent {
        ValidatedEvent {
            event_id: EventId::new(),
            source: EventSource::Cli,
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: Some("cli:local".into()),
            },
            session_target: SessionTarget {
                agent_id: AgentId("main".into()),
                channel: ChannelKind::Cli,
                conversation_key: "local".into(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text: "hi".into(),
                message_id: None,
                chat_id: None,
            },
            dedupe_key: format!("dedupe-{}", uuid::Uuid::new_v4()),
            occurred_at: chrono::Utc::now(),
        }
    }
    fn test_config() -> KernelConfig {
        // Reuse _cfg() but point root_dir at a nonexistent path so the Context
        // assembler uses safe fallback text (exercises the no-chat-only path).
        let mut c = super::super::tool_loop_tests::test_config();
        c.root_dir = PathBuf::from("/nonexistent-agent-core-root-xyz");
        c
    }
    fn build_blocks(grants: &[String]) -> Vec<ContextBlock> {
        let cfg = test_config();
        let journal = JournalStore::in_memory().unwrap();
        let session = Session {
            id: SessionId("s1".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "local".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: chrono::Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };
        let event = empty_event(&session);
        let snap = crate::registry::snapshot::test_snapshot();
        ContextAssembler::from_config(&cfg)
            .build(&journal, &session, &event, "hi", grants, &snap)
            .unwrap()
    }
    fn catalog_block_text(blocks: &[ContextBlock]) -> String {
        blocks
            .iter()
            .find(|b| matches!(b.kind, ContextBlockKind::ToolCatalog))
            .map(|b| b.content.clone())
            .unwrap_or_default()
    }
    #[test]
    fn context_tool_catalog_omits_ungranted_time_now() {
        // No system.status grant → the ToolCatalog block must not mention it.
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let blocks = build_blocks(&grants);
        let cat = catalog_block_text(&blocks);
        assert!(
            !cat.contains("system.status"),
            "ToolCatalog must omit un-granted system.status: {cat}"
        );
    }
    #[test]
    fn context_tool_catalog_includes_granted_system_status() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&["system.status".to_string()])
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let blocks = build_blocks(&grants);
        let cat = catalog_block_text(&blocks);
        assert!(
            cat.contains("system.status"),
            "granted system.status must be listed: {cat}"
        );
        // Write ops never listed even when granted.
        assert!(!cat.contains("feishu.send_message"));
        assert!(!cat.contains("stdout.send_text"));
    }
    #[test]
    fn context_fallback_contains_no_chat_only_semantics() {
        // When prompt files are absent (root_dir points nowhere), the context
        // uses safe fallback text that must NOT re-introduce Phase-0 semantics.
        let blocks = build_blocks(&[]);
        let all = blocks
            .iter()
            .map(|b| b.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains("chat-only"), "fallback leaked chat-only");
        assert!(!all.contains("Phase 0"), "fallback leaked Phase 0");
        assert!(
            !all.contains("without tools"),
            "fallback leaked without tools"
        );
    }
    #[test]
    fn context_fallback_does_not_leak_paths_or_errors() {
        let blocks = build_blocks(&[]);
        let all = blocks
            .iter()
            .map(|b| b.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !all.contains("nonexistent-agent-core-root"),
            "fallback leaked a file path: {all}"
        );
        assert!(
            !all.contains("No such file") && !all.contains("os error"),
            "fallback leaked an I/O error"
        );
    }
} // end mod grants_context_tests
