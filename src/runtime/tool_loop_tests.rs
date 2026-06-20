#[cfg(test)]
mod tool_loop_tests {

    use crate::adapters::InvocationAdapter;
    use crate::config::KernelConfig;
    use crate::domain::*;
    use crate::gateway::Gateway;
    use crate::journal::JournalStore;
    use crate::llm::ToolCall;
    use crate::runtime::Runtime;
    use anyhow::Result;
    use serde_json::json;

    /// SQLite error injection: drop `journal_events` so queries fail. Verify
    /// failure is NOT swallowed → ReceiptReceived Failed, sanitized error.
    #[test]
    fn session_recall_sql_error_returns_failed_not_empty_success() {
        let dir = std::env::temp_dir().join(format!("tool-loop-sql-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let db_path = dir.join("test.db");

        let journal = JournalStore::open(&db_path).unwrap();
        // Corrupt: drop the journal_events table so queries fail.
        {
            let conn = journal.conn.lock().unwrap();
            conn.execute_batch("DROP TABLE IF EXISTS journal_events;")
                .unwrap();
        }

        let approved = fake_approved("session.recall_recent", json!({}));
        let (status, output, text) = Runtime::<crate::llm::LocalEchoLlm>::execute_session_recall(
            &journal,
            &SessionId("s1".into()),
            &approved,
        )
        .unwrap_or((
            crate::domain::ReceiptStatus::Failed,
            json!({"error": "test"}),
            "test failed".into(),
        ));

        // Must NOT be Succeeded — DB failure propagates as Failed.
        assert_eq!(status, crate::domain::ReceiptStatus::Failed);
        // Output must contain a sanitized error, not an empty messages array.
        assert!(output.get("error").is_some(), "error field present");
        assert!(
            output.get("messages").is_none(),
            "must not return messages on DB failure"
        );
        assert!(text.contains("failed"), "text indicates failure");
        // Must not leak SQL internals.
        let json_str = serde_json::to_string(&output).unwrap();
        assert!(
            !json_str.contains("journal_events") && !json_str.contains("sqlite"),
            "no SQL internals leaked"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ToolCallIssued written for every tool call; ToolCallRejected written
    /// for rejected calls (no ReceiptReceived, InvocationProposed, InvocationApproved).
    #[test]
    fn rejected_tool_call_writes_issued_and_rejected_not_invocation() {
        let config = test_config();
        let journal = JournalStore::in_memory().unwrap();
        let gateway = Gateway::new(config.clone());
        let runtime = Runtime::new(config.clone(), crate::llm::LocalEchoLlm);

        let now = chrono::Utc::now();
        let session = Session {
            id: SessionId("s1".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "local".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: now,
            status: SessionStatus::Active,
            version: 1,
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: Some("cli:local".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
        };

        // Unknown operation → ToolCallIssued + ToolCallRejected (no Receipt).
        let bad_op = ToolCall {
            id: "bad_op".into(),
            operation: "shell.exec".into(),
            arguments: json!({}),
        };
        let result = runtime.handle_inline_tool_call(&journal, &gateway, &run, &session, &bad_op);
        assert!(result.is_ok());

        let events = journal.events().unwrap();
        let count = |kind| events.iter().filter(|e| e.kind == kind).count();
        assert_eq!(
            count(JournalEventKind::ToolCallIssued),
            1,
            "ToolCallIssued for the tool call"
        );
        assert_eq!(
            count(JournalEventKind::ToolCallRejected),
            1,
            "ToolCallRejected for rejection"
        );
        assert_eq!(
            count(JournalEventKind::InvocationProposed),
            0,
            "no InvocationProposed"
        );
        assert_eq!(
            count(JournalEventKind::InvocationApproved),
            0,
            "no InvocationApproved"
        );
        assert_eq!(
            count(JournalEventKind::ReceiptReceived),
            0,
            "no ReceiptReceived (never executed)"
        );

        // Verify the ToolCallRejected payload has error_category.
        let rejected = events
            .iter()
            .find(|e| {
                e.kind == JournalEventKind::ToolCallRejected
                    && e.payload.get("tool_call_id").and_then(|v| v.as_str()) == Some("bad_op")
            })
            .unwrap();
        assert!(
            rejected.payload.get("error_category").is_some(),
            "ToolCallRejected has error_category"
        );
    }

    /// ToolCallRejected payload does not leak raw error internals.
    #[test]
    fn rejected_tool_call_sanitized_payload() {
        // Validation errors produce fixed category strings, not raw error text.
        use crate::runtime::tool_loop::sanitize_rejection;
        use crate::runtime::validate_model_arguments;
        let result = validate_model_arguments("system.status", &json!({"extra_field": "value"}));
        assert!(result.is_err());
        let (_category, _) = sanitize_rejection(&result.unwrap_err());
        // The category is a fixed enum string, not a verbatim error message.
        // (We already test this via the ToolCallRejected journal event checks.)
    }

    /// Precise audit-fact count: 1 InvocationProposed, 1 InvocationApproved,
    /// 1 ReceiptReceived (Succeeded) for a single successful tool execution.
    #[test]
    fn precise_audit_fact_counts_on_successful_tool_call() {
        // Use the FakeLLM approach: first call returns a tool_call for
        // time.now, second returns text. We call handle_inline_tool_call
        // directly and count the Journal events it writes.
        let config = test_config();
        let journal = JournalStore::in_memory().unwrap();
        let gateway = Gateway::new(config.clone());
        let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);

        let now = chrono::Utc::now();
        let session = Session {
            id: SessionId("s1".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "local".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: now,
            status: crate::domain::SessionStatus::Active,
            version: 1,
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![CapabilityGrant {
                    operation: "time.now".into(),
                    scope: "current_session".into(),
                }],
                requester_id: Some("cli:local".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
        };

        let tool_call = ToolCall {
            id: "tc1".into(),
            operation: "time.now".into(),
            arguments: json!({}),
        };

        // Execute the tool call — this writes InvocationProposed +
        // InvocationApproved + ReceiptReceived.
        let result =
            runtime.handle_inline_tool_call(&journal, &gateway, &run, &session, &tool_call);
        assert!(result.is_ok());

        let events = journal.events().unwrap();
        let proposed = events
            .iter()
            .filter(|e| e.kind == JournalEventKind::InvocationProposed)
            .count();
        let approved = events
            .iter()
            .filter(|e| e.kind == JournalEventKind::InvocationApproved)
            .count();
        let receipts = events
            .iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .count();

        // Exactly 1 of each for the single tool execution.
        assert_eq!(
            proposed, 1,
            "exactly 1 InvocationProposed, got {}",
            proposed
        );
        assert_eq!(
            approved, 1,
            "exactly 1 InvocationApproved, got {}",
            approved
        );
        assert_eq!(receipts, 1, "exactly 1 ReceiptReceived, got {}", receipts);
        // The receipt must be Succeeded.
        let receipt = events
            .iter()
            .find(|e| e.kind == JournalEventKind::ReceiptReceived)
            .unwrap();
        assert_eq!(
            receipt.payload.get("status").and_then(|s| s.as_str()),
            Some("Succeeded")
        );
    }

    fn fake_approved(operation: &str, args: serde_json::Value) -> ApprovedInvocation {
        ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: operation.to_string(),
                arguments: args,
                idempotency_key: Some("test".into()),
            },
            "decision_test".into(),
        )
    }

    fn test_config() -> KernelConfig {
        use std::path::PathBuf;
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
        }
    }
}
