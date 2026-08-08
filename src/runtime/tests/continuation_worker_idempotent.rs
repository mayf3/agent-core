//! Continuation governance and lifecycle fail-closed regressions.
//!
//! The continuation worker uses the PRE-ALLOCATED next_run_id from the
//! ledger. It may create a missing Run, but it must never recover or replay a
//! partial/incomplete Run. Governance conflicts must be rejected before a Run,
//! model invocation, or tool call is created.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::{Gateway, SessionContinuationRequest};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCallResult};
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
        force_legacy_runtime: false,
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

fn setup() -> Result<(
    JournalStore,
    Runtime<ReplyLlm>,
    Gateway,
    Run,
    Session,
    Arc<AtomicUsize>,
)> {
    let journal = JournalStore::in_memory()?;
    let snapshot = journal.create_registry_snapshot_with_hook_bindings(
        crate::registry::store::builtin_specs(),
        crate::registry::store::builtin_hook_bindings(),
    )?;
    let session = journal.get_or_create_session(&SessionTarget {
        agent_id: AgentId("agent-frozen".into()),
        channel: ChannelKind::Cli,
        conversation_key: "local".into(),
    })?;
    let mut trigger = make_trigger(&snapshot.snapshot_id);
    trigger.session_id = session.id.clone();
    trigger.agent_id = session.agent_id.clone();
    journal.insert_run(&trigger)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::new(
        test_config(),
        ReplyLlm {
            calls: Arc::clone(&calls),
        },
    );
    let gateway = Gateway::new(runtime.config().clone());
    Ok((journal, runtime, gateway, trigger, session, calls))
}

fn count_events(journal: &JournalStore, kind: JournalEventKind) -> Result<usize> {
    Ok(journal
        .events()?
        .into_iter()
        .filter(|event| event.kind == kind)
        .count())
}

fn continuation_request(trigger: &Run) -> SessionContinuationRequest {
    SessionContinuationRequest {
        trigger_run_id: trigger.id.0.clone(),
        expected_session_id: Some(trigger.session_id.0.clone()),
        idempotency_key: format!("continuation:{}", trigger.id.0),
    }
}

/// A missing Run is created and started once. A retry while it is still
/// non-terminal is stranded and must not invoke the model or tools again.
#[test]
fn incomplete_started_run_fails_closed_without_duplicate_calls() -> Result<()> {
    let (journal, runtime, gateway, trigger, session, calls) = setup()?;
    let preallocated = RunId("run_next_preallocated".into());

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
    assert!(journal.run_has_started(&preallocated)?);
    let model_calls_before = calls.load(Ordering::SeqCst);
    let tool_calls_before = count_events(&journal, JournalEventKind::ToolCallIssued)?;

    let retry = runtime.schedule_run_for_existing_session(
        &journal,
        &gateway,
        &trigger,
        &session,
        &trigger.id,
        &preallocated,
    );
    let error = retry
        .err()
        .expect("incomplete started Run must fail closed");
    assert!(error.to_string().contains("continuation_run_stranded"));
    assert_eq!(calls.load(Ordering::SeqCst), model_calls_before);
    assert_eq!(
        count_events(&journal, JournalEventKind::ToolCallIssued)?,
        tool_calls_before
    );
    let started = journal
        .events()?
        .into_iter()
        .filter(|e| {
            e.kind == JournalEventKind::RunStarted && e.run_id.as_ref() == Some(&preallocated)
        })
        .count();
    assert_eq!(started, 1, "RunStarted emitted once");
    let anomaly = journal.events()?.into_iter().any(|event| {
        event.kind == JournalEventKind::RunFailed
            && event.run_id.as_ref() == Some(&preallocated)
            && event.payload["error_category"] == json!("continuation_run_stranded")
    });
    assert!(anomaly, "stranded Run anomaly recorded");
    Ok(())
}

/// A legacy partial Run row from the old crash window is never started or
/// replayed. It is failed closed and the anomaly is recorded.
#[test]
fn partial_run_row_fails_closed_without_model_or_tool_calls() -> Result<()> {
    let (journal, runtime, gateway, trigger, session, calls) = setup()?;
    let partial = Run {
        id: RunId("run_next_partial".into()),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: EventId::new(),
        registry_snapshot_id: trigger.registry_snapshot_id.clone(),
        ..trigger.clone()
    };
    journal.insert_run(&partial)?;
    let result = runtime.schedule_run_for_existing_session(
        &journal,
        &gateway,
        &trigger,
        &session,
        &trigger.id,
        &partial.id,
    );
    let error = result.err().expect("partial Run row must fail closed");
    assert!(error.to_string().contains("continuation_run_partial"));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no duplicate model call");
    assert_eq!(count_events(&journal, JournalEventKind::ToolCallIssued)?, 0);
    assert!(!journal.run_has_started(&partial.id)?);
    let anomaly = journal.events()?.into_iter().any(|event| {
        event.kind == JournalEventKind::RunFailed
            && event.run_id.as_ref() == Some(&partial.id)
            && event.payload["error_category"] == json!("continuation_run_partial")
    });
    assert!(anomaly, "partial Run anomaly recorded");
    Ok(())
}

/// A clearly terminal existing Run is returned without re-execution.
#[test]
fn terminal_existing_run_is_returned_without_duplicate_calls() -> Result<()> {
    let (journal, runtime, gateway, trigger, session, calls) = setup()?;
    let terminal = Run {
        id: RunId("run_next_terminal".into()),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: EventId::new(),
        registry_snapshot_id: trigger.registry_snapshot_id.clone(),
        status: RunStatus::Completed,
        ..trigger.clone()
    };
    journal.insert_run_and_start(
        &terminal,
        &session.id,
        "continuation_terminal_event",
        &trigger.id,
    )?;
    let outcome = runtime.schedule_run_for_existing_session(
        &journal,
        &gateway,
        &trigger,
        &session,
        &trigger.id,
        &terminal.id,
    )?;
    assert_eq!(outcome.run_id, terminal.id);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(count_events(&journal, JournalEventKind::ToolCallIssued)?, 0);
    Ok(())
}

/// A caller-supplied Session inconsistent with the trigger is rejected before
/// the pre-allocated Run, model invocation, or tool call exists.
#[test]
fn trigger_session_mismatch_fails_closed() -> Result<()> {
    let (journal, runtime, gateway, trigger, mut session, calls) = setup()?;
    session.id = SessionId("s_conflict".into());
    let next_run_id = RunId("run_must_not_exist_session".into());
    let error = runtime
        .schedule_run_for_existing_session(
            &journal,
            &gateway,
            &trigger,
            &session,
            &trigger.id,
            &next_run_id,
        )
        .err()
        .expect("trigger/session mismatch must fail closed");
    assert!(error.to_string().contains("continuation_session_mismatch"));
    assert!(journal.run_by_id(&next_run_id)?.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(count_events(&journal, JournalEventKind::RunStarted)?, 0);
    assert_eq!(
        count_events(&journal, JournalEventKind::ModelInvocationStarted)?,
        0
    );
    assert_eq!(count_events(&journal, JournalEventKind::ToolCallIssued)?, 0);
    Ok(())
}

/// A persisted trigger/session agent conflict is rejected by the Gateway
/// before continuation ledger/event/worker acceptance.
#[test]
fn trigger_agent_session_mismatch_fails_closed() -> Result<()> {
    let (journal, _runtime, gateway, mut trigger, _session, calls) = setup()?;
    trigger.id = RunId("run_trigger_agent_conflict".into());
    trigger.trigger_event_id = EventId::new();
    trigger.agent_id = AgentId("agent-conflict".into());
    journal.insert_run(&trigger)?;
    let error = gateway
        .request_session_continuation(&journal, &continuation_request(&trigger))
        .expect_err("trigger/session agent mismatch must fail closed");
    assert!(error.to_string().contains("continuation_agent_mismatch"));
    assert!(journal.continuation_by_trigger_run(&trigger.id)?.is_none());
    assert_eq!(
        count_events(&journal, JournalEventKind::SessionContinuationRequested)?,
        0
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(count_events(&journal, JournalEventKind::RunStarted)?, 0);
    assert_eq!(
        count_events(&journal, JournalEventKind::ModelInvocationStarted)?,
        0
    );
    assert_eq!(count_events(&journal, JournalEventKind::ToolCallIssued)?, 0);
    Ok(())
}

/// The only principal fact directly comparable to the current Session model
/// is generic transport source/channel. A conflict fails closed without adding
/// product-specific identity rules.
#[test]
fn comparable_principal_source_conflict_fails_closed() -> Result<()> {
    let (journal, _runtime, gateway, mut trigger, _session, calls) = setup()?;
    trigger.id = RunId("run_trigger_principal_conflict".into());
    trigger.trigger_event_id = EventId::new();
    trigger.principal.source = PrincipalSource::Feishu;
    journal.insert_run(&trigger)?;
    let error = gateway
        .request_session_continuation(&journal, &continuation_request(&trigger))
        .expect_err("principal source/session channel mismatch must fail closed");
    assert!(error
        .to_string()
        .contains("continuation_principal_mismatch"));
    assert!(journal.continuation_by_trigger_run(&trigger.id)?.is_none());
    assert_eq!(
        count_events(&journal, JournalEventKind::SessionContinuationRequested)?,
        0
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(count_events(&journal, JournalEventKind::RunStarted)?, 0);
    assert_eq!(
        count_events(&journal, JournalEventKind::ModelInvocationStarted)?,
        0
    );
    assert_eq!(count_events(&journal, JournalEventKind::ToolCallIssued)?, 0);
    Ok(())
}
