//! A, B, F, G — Registry snapshot lifecycle integration tests.
//!
//! A  — Provider tools are pinned to the Run's snapshot (not current/live).
//! B  — Context ToolCatalog is pinned to the Run's snapshot.
//! F  — Restart recovery preserves snapshot binding.
//! G1 — Current snapshot missing → deliver fails cleanly.
//! G2 — Current snapshot ID points to nonexistent snapshot → deliver fails cleanly.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;

// ---- Fixtures ----

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
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        capability_submit_token: None,
        capability_decision_token: None,
    }
}

fn v1_specs() -> Vec<OperationSpec> {
    vec![
        OperationSpec {
            name: "stdout.send_text".into(),
            risk: Risk::Write,
            description: "send reply".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.stdout_send_text".into(),
        },
        OperationSpec {
            name: "system.status".into(),
            risk: Risk::ReadOnly,
            description: "v1 description".into(),
            parameters: json!({"type": "object", "v1_marker": true, "additionalProperties": false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.system_status".into(),
        },
    ]
}

// ========================================================================

#[test]
fn f_restart_recovery_preserves_snapshot_binding() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("reg-snap-f-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("test.sqlite");

    let run_id;
    let v1_snapshot_id;

    // Session 1: create v1 snapshot, create Run A.
    {
        let journal = JournalStore::open(&db_path)?;
        journal.initialize_registry()?;
        let config = test_config();
        let gateway = Gateway::new(config.clone());

        let snap_v1 = journal.create_registry_snapshot(v1_specs())?;
        v1_snapshot_id = snap_v1.snapshot_id.clone();
        journal.activate_registry_snapshot(&v1_snapshot_id)?;

        let llm = crate::llm::LocalEchoLlm;
        let runtime = Runtime::new(config, llm);
        let envelope = gateway.cli_ingress("test".into())?;
        let event = gateway.validate_ingress(&journal, envelope)?;
        let result = runtime.deliver(&journal, &gateway, event)?;
        run_id = result.run_id;
    }

    // Session 2: reopen, verify snapshot binding.
    {
        let journal = JournalStore::open(&db_path)?;
        journal.initialize_registry()?;

        let run = journal.run(&run_id)?.expect("Run must exist after reopen");
        assert_eq!(
            run.registry_snapshot_id, v1_snapshot_id,
            "registry_snapshot_id must survive restart"
        );

        let snap = journal.load_registry_snapshot(&run.registry_snapshot_id)?;
        assert_eq!(snap.snapshot_id, v1_snapshot_id);

        // Verify provider tools would produce v1.
        let granted: Vec<String> = run
            .principal
            .grants
            .iter()
            .map(|g| g.operation.clone())
            .collect();
        let tools = snap.provider_tools_for_grants(&granted);
        let desc = tools
            .iter()
            .find(|t| t.pointer("/function/name").and_then(Value::as_str) == Some("system.status"))
            .and_then(|t| t.pointer("/function/description").and_then(Value::as_str))
            .unwrap_or("");
        assert_eq!(
            desc, "v1 description",
            "Provider tools must be v1 after restart"
        );

        // Verify Context would use v1.
        let config = test_config();
        let blocks = crate::context::ContextAssembler::from_config(&config).build(
            &journal,
            &journal.get_or_create_session(&SessionTarget {
                agent_id: config.agent_id.clone(),
                channel: ChannelKind::Cli,
                conversation_key: "local".into(),
            })?,
            &crate::domain::ValidatedEvent {
                event_id: EventId::new(),
                source: EventSource::Cli,
                principal: run.principal.clone(),
                session_target: SessionTarget {
                    agent_id: config.agent_id.clone(),
                    channel: ChannelKind::Cli,
                    conversation_key: "local".into(),
                },
                payload: RuntimeEventPayload::UserMessage {
                    text: "hi".into(),
                    message_id: None,
                    chat_id: None,
                },
                dedupe_key: "restart-test".into(),
                occurred_at: chrono::Utc::now(),
            },
            "hi",
            &granted,
            &snap,
        )?;
        let cat = blocks
            .iter()
            .find(|b| matches!(b.kind, ContextBlockKind::ToolCatalog))
            .map(|b| b.content.as_str())
            .unwrap_or("");
        assert!(cat.contains("v1 description"), "Context must be v1: {cat}");
        assert!(!cat.contains("v2 description"), "Context must NOT be v2");

        // Verify Gateway lookup uses v1.
        let spec = snap.lookup("system.status").unwrap();
        assert_eq!(
            spec.risk,
            Risk::ReadOnly,
            "Gateway must read ReadOnly from v1 snapshot"
        );
        assert_eq!(
            spec.binding_key, "builtin.system_status",
            "binding_key must come from v1 snapshot"
        );

        // === ACTIVATE v2, PROVE RESTORED RUN STAYS ON v1 ===
        // Create a v2 snapshot with a distinctly different description.
        let v2_specs = vec![
            OperationSpec {
                name: "stdout.send_text".into(),
                risk: Risk::Write,
                description: "send reply".into(),
                parameters: json!({"type": "object"}),
                idempotent: false,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.stdout_send_text".into(),
            },
            OperationSpec {
                name: "system.status".into(),
                risk: Risk::ReadOnly,
                description: "v2 description".into(),
                parameters: json!({"type": "object", "v2_marker": true, "additionalProperties": false}),
                idempotent: true,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.system_status".into(),
            },
        ];
        let snap_v2 = journal.create_registry_snapshot(v2_specs)?;
        let v2_snapshot_id = snap_v2.snapshot_id.clone();
        journal.activate_registry_snapshot(&v2_snapshot_id)?;

        // Frozen assertion 1: current has switched to v2.
        assert_eq!(
            journal.current_registry_snapshot_id()?,
            v2_snapshot_id,
            "current must be v2 after activation"
        );

        // Frozen assertion 2: v1 and v2 IDs must differ.
        assert_ne!(
            v1_snapshot_id, v2_snapshot_id,
            "v1 and v2 snapshot IDs must differ"
        );

        // Frozen assertion 3: restored Run still fixed to v1.
        assert_eq!(
            run.registry_snapshot_id, v1_snapshot_id,
            "restored Run's registry_snapshot_id must remain v1"
        );

        // Verify v2 content is different from v1.
        let current_snap = journal.load_registry_snapshot(&v2_snapshot_id)?;
        assert_eq!(
            current_snap.lookup("system.status").unwrap().description,
            "v2 description",
            "current (v2) snapshot must have v2 description"
        );

        // Verify restored Run still loads v1 content.
        let restored_snap = journal.load_registry_snapshot(&run.registry_snapshot_id)?;
        assert_eq!(
            restored_snap.lookup("system.status").unwrap().description,
            "v1 description",
            "restored Run's snapshot must have v1 description"
        );

        // === REAL GATEWAY AND DISPATCH ===
        // Use the restored Run + snapshot (still v1) to validate a real
        // tool call through the production Gateway and dispatch pipeline.
        // Even though current=v2, the restored Run's binding is pinned to v1.
        use crate::gateway::validate_tool_call;
        use crate::llm::tool_call_id_hash;

        let hashed_id = tool_call_id_hash("call_restored_time");
        let tool_call = crate::llm::ToolCall {
            id: hashed_id,
            operation: "system.status".into(),
            arguments: json!({}),
        };

        // 1. validate_tool_call — uses loaded snapshot for existence + risk.
        let intent = validate_tool_call(&tool_call, &run.id, 0, 0, &snap)
            .expect("system.status must validate against restored v1 snapshot");
        assert_eq!(
            intent.operation, "system.status",
            "Approved operation must be system.status"
        );

        // 2. Gateway::approve_invocation — uses restored Run grants + snapshot.
        let session = journal.get_or_create_session(&SessionTarget {
            agent_id: run.agent_id.clone(),
            channel: ChannelKind::Cli,
            conversation_key: run.session_id.0.clone(),
        })?;
        let approved = crate::gateway::Gateway::new(test_config()).approve_invocation(
            crate::domain::InvocationIntent {
                invocation_id: intent.invocation_id,
                run_id: run.id.clone(),
                operation: intent.operation.clone(),
                arguments: json!({"session_id": session.id.0}),
                idempotency_key: intent.idempotency_key.clone(),
            },
            &run,
            &session,
            &snap,
        )?;
        assert_eq!(
            approved.intent().operation,
            "system.status",
            "Approved operation must be system.status"
        );

        // 3+4. Production inline dispatch via dispatch_builtin_binding.
        //     This is the SINGLE authoritative binding_key → handler match
        //     that the tool loop also calls. The same code path handles
        //     ReceiptReceived journal recording.
        let correlation_id = approved.intent().invocation_id.0.clone();
        let outcome = crate::runtime::tool_execution::dispatch_builtin_binding(
            &spec,
            &approved,
            &journal,
            &run,
            &session,
            &correlation_id,
            std::time::Duration::from_millis(10_000),
        );
        let out_text = match &outcome {
            crate::runtime::tool_loop::ToolCallOutcome::ToolResult { text } => text.clone(),
            _ => String::new(),
        };
        assert!(
            !out_text.is_empty(),
            "expected ToolResult, got non-ToolResult"
        );
        assert!(
            out_text.contains("succeeded"),
            "outcome must indicate success: {out_text}"
        );

        // Verify ReceiptReceived was written to journal for this run/invocation.
        let receipt_events: Vec<_> = journal
            .events()
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.kind == JournalEventKind::ReceiptReceived
                    && e.run_id == Some(run.id.clone())
                    && e.payload.get("invocation_id").and_then(Value::as_str)
                        == Some(&approved.intent().invocation_id.0)
            })
            .collect();
        assert_eq!(
            receipt_events.len(),
            1,
            "exactly one ReceiptReceived must exist for run_id={:?} invocation={}",
            run.id,
            approved.intent().invocation_id.0
        );
        let receipt = &receipt_events[0];
        assert_eq!(
            receipt.payload.get("status").and_then(Value::as_str),
            Some("Succeeded"),
            "Receipt must be Succeeded"
        );
        assert!(
            receipt
                .payload
                .pointer("/output/status")
                .and_then(Value::as_str)
                .is_some(),
            "output must contain 'status' (via system.status handler binding)"
        );
        assert!(
            receipt
                .payload
                .pointer("/output/event_count")
                .and_then(Value::as_number)
                .is_some(),
            "output must contain 'event_count' (via system.status handler binding)"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
