//! High 4: worker retry idempotency for the same-session continuation path.
//!
//! The continuation worker uses the PRE-ALLOCATED next_run_id from the
//! ledger. When the worker retries (crash before Run creation, crash after
//! Run creation before ack, worker re-lease), `schedule_run_for_existing_session`
//! must:
//!   - Run missing  → create it once;
//!   - Run present with consistent facts → treat as success (no second Run);
//!   - Run present with conflicting facts → fail closed.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn test_config() -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: PathBuf::from("."),
        kernel_port: 0,
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
        model_timeout_ms: 30_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        max_tool_rounds: 2,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
        budget_hook: crate::hook::HookConfig::default(),
    }
}

/// Fake LLM that always returns a final reply (no tool call) so the Run
/// completes normally and never yields.
struct ReplyLlm {
    calls: Arc<AtomicUsize>,
}

impl LlmClient for ReplyLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmOutput {
            provider: "t".into(),
            model: "t".into(),
            content: "done".into(),
            journal_payload: json!({}),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}


fn make_trigger(snapshot_id: &str) -> Run {
    Run {
        id: RunId("run_trigger_worker".into()),
        session_id: SessionId("s_cont_worker".into()),
        agent_id: AgentId("agent-frozen".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:local".into()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![
                CapabilityGrant {
                    operation: "stdout.send_text".to_string(),
                    scope: "current_session".to_string(),
                },
                CapabilityGrant {
                    operation: "system.status".to_string(),
                    scope: "current_session".to_string(),
                },
            ],
            requester_id: Some("cli:local".into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: snapshot_id.to_string(),
        mode: RunMode::Default,
        budget_hook_id: None,
        budget_hook_version: None,
        budget_decision_digest: None,
        budget_max_tool_rounds: None,
        budget_max_wall_time_ms: None,
        budget_exhaustion_action: None,
    }
}

fn make_session() -> Session {
    Session {
        id: SessionId("s_cont_worker".into()),
        agent_id: AgentId("agent-frozen".into()),
        channel: ChannelKind::Cli,
        conversation_key: "local".into(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: chrono::Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    }
}

/// The worker uses the PRE-ALLOCATED next_run_id; calling
/// `schedule_run_for_existing_session` twice with the same pre-allocated id
/// creates the Run ONCE (retry after crash/ack loss is idempotent).
#[test]
fn worker_retry_with_preallocated_run_id_creates_once() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let snapshot = journal
        .create_registry_snapshot_with_hook_bindings(
            crate::registry::store::builtin_specs(),
            crate::registry::store::builtin_hook_bindings(),
        )
        .unwrap();
    let snapshot_id = snapshot.snapshot_id.clone();
    let trigger = make_trigger(&snapshot_id);
    journal.insert_run(&trigger)?;
    let session = make_session();
    journal.get_or_create_session(&SessionTarget {
        agent_id: AgentId("agent-frozen".into()),
        channel: ChannelKind::Cli,
        conversation_key: "local".into(),
    })?;
    let config = test_config();
    let runtime = Runtime::new(config, ReplyLlm {
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let gateway = Gateway::new(runtime.config().clone());
    let preallocated = RunId("run_next_preallocated".into());

    // First call: creates the Run.
    let first = runtime.schedule_run_for_existing_session(
        &journal,
        &gateway,
        &trigger,
        &session,
        &trigger.id,
        &preallocated,
    )?;
    assert_eq!(first.run_id, preallocated);
    let runs = journal
        .run_by_id(&preallocated)?
        .expect("pre-allocated Run created");
    assert_eq!(runs.agent_id.0, "agent-frozen", "frozen agent_id inherited");

    // Second call (worker retry after ack loss): Run already exists with
    // consistent facts → treated as success, NOT created twice.
    let second = runtime.schedule_run_for_existing_session(
        &journal,
        &gateway,
        &trigger,
        &session,
        &trigger.id,
        &preallocated,
    )?;
    assert_eq!(second.run_id, preallocated);
    let started = journal
        .events()?
        .into_iter()
        .filter(|e| {
            e.kind == JournalEventKind::RunStarted && e.run_id.as_ref() == Some(&preallocated)
        })
        .count();
    assert_eq!(started, 1, "WORKER_RETRY_IDEMPOTENT — RunStarted emitted once");
    Ok(())
}

/// A conflicting pre-allocated Run (different session) fails closed.
#[test]
fn worker_conflicting_preallocated_run_fails_closed() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let snapshot = journal
        .create_registry_snapshot_with_hook_bindings(
            crate::registry::store::builtin_specs(),
            crate::registry::store::builtin_hook_bindings(),
        )
        .unwrap();
    let snapshot_id = snapshot.snapshot_id.clone();
    let trigger = make_trigger(&snapshot_id);
    journal.insert_run(&trigger)?;
    let session = make_session();
    journal.get_or_create_session(&SessionTarget {
        agent_id: AgentId("agent-frozen".into()),
        channel: ChannelKind::Cli,
        conversation_key: "local".into(),
    })?;
    // Pre-existing Run with the pre-allocated id but a DIFFERENT session.
    let conflicting = Run {
        id: RunId("run_next_conflict".into()),
        session_id: SessionId("s_other".into()),
        ..make_trigger(&snapshot_id)
    };
    journal.insert_run(&conflicting)?;
    let config = test_config();
    let runtime = Runtime::new(config, ReplyLlm {
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let gateway = Gateway::new(runtime.config().clone());
    let result = runtime.schedule_run_for_existing_session(
        &journal,
        &gateway,
        &trigger,
        &session,
        &trigger.id,
        &conflicting.id,
    );
    assert!(
        result.is_err(),
        "conflicting pre-allocated Run must fail closed"
    );
    Ok(())
}
