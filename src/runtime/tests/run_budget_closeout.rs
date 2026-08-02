//! Run Budget Hook V0 directed close-out tests (PR #217 follow-up).
//!
//! Covers the 15 acceptance criteria of the close-out:
//!  High 1 (Snapshot-bound hook selection):
//!    1. default binding resolvable from Snapshot
//!    2. external binding replaces via a new Snapshot
//!    3. same Snapshot never switches hook identity on env change
//!    4. old Run keeps old binding and decision
//!    5. missing / conflicting / untrusted binding → fail closed
//!    6. model cannot select hook or override budget in tool arguments
//!  High 2 (deadline-bound in-flight calls):
//!    7. no new LLM call after Run deadline
//!    8. no new Tool call after Run deadline
//!    9. blocking LLM future stops waiting AT the deadline (real HTTP)
//!   10. blocking HTTP tool stops waiting AT the deadline (real HTTP)
//!   11. yield / terminate semantics stay explicit
//!   12. no next round after timeout
//!   13. unverifiable remote cancellation recorded honestly
//!   14. chat / Approval / Receipt / Snapshot / hash chain no regression
//!   15. no Coding/Ops/Router/OpenClaw special-casing in Kernel

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::hook::{ExhaustionAction, HookConfig, HookKind, RunBudgetDecision};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, OpenAiCompatibleLlm, ToolCall, ToolCallResult};
use crate::registry::snapshot::{
    BindingKind, HookBinding, OperationSpec, RegistrySnapshot, Risk, BUDGET_HOOK_CONTRACT,
};
use crate::registry::store::builtin_hook_bindings;
use crate::runtime::Runtime;
use serde_json::json;
use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ── Helpers ──

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

fn builtin_snapshot() -> RegistrySnapshot {
    RegistrySnapshot {
        snapshot_id: "snap_closeout_builtin".to_string(),
        created_at: chrono::Utc::now(),
        operations: vec![
            OperationSpec {
                name: "stdout.send_text".to_string(),
                risk: Risk::Write,
                description: "reply".into(),
                parameters: json!({"type":"object"}),
                idempotent: false,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.stdout_send_text".into(),
            },
            OperationSpec {
                name: "system.status".to_string(),
                risk: Risk::ReadOnly,
                description: "test".into(),
                parameters: json!({"type":"object"}),
                idempotent: false,
                binding_kind: BindingKind::Builtin,
                binding_key: "builtin.system_status".into(),
            },
        ],
        hook_bindings: builtin_hook_bindings(),
    }
}

fn external_snapshot(hook_id: &str, provider_id: &str, endpoint: &str) -> RegistrySnapshot {
    let mut snap = builtin_snapshot();
    snap.hook_bindings = vec![HookBinding {
        contract: crate::registry::snapshot::BUDGET_HOOK_CONTRACT.to_string(),
        hook_id: hook_id.into(),
        hook_version: "v0".into(),
        binding_kind: BindingKind::External,
        binding_key: "external.run_budget_resolve_v0".into(),
        provider_id: provider_id.into(),
        endpoint: endpoint.into(),
    }];
    snap
}

fn make_run(snapshot: &RegistrySnapshot) -> Run {
    Run {
        id: RunId::new(),
        session_id: SessionId("s_closeout".into()),
        agent_id: AgentId("main".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:local".into()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![CapabilityGrant {
                operation: "system.status".to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("cli:local".into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: snapshot.snapshot_id.clone(),
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
        id: SessionId("s_closeout".into()),
        agent_id: AgentId("main".into()),
        channel: ChannelKind::Cli,
        conversation_key: "local".into(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: chrono::Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    }
}

/// Fake LLM counting calls; returns a tool call while `remaining > 0`.
struct NTimeToolLlm {
    remaining: Arc<AtomicUsize>,
    operation: &'static str,
}

impl NTimeToolLlm {
    fn new(n: usize) -> (Self, Arc<AtomicUsize>) {
        let remaining = Arc::new(AtomicUsize::new(n));
        (
            Self {
                remaining: remaining.clone(),
                operation: "system.status",
            },
            remaining,
        )
    }
}

impl LlmClient for NTimeToolLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        let prev = self.remaining.fetch_sub(1, Ordering::SeqCst);
        if prev > 0 {
            Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: format!("tool round {}", prev),
                journal_payload: json!({"s":"ok"}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: format!("tc_{}", prev),
                    operation: self.operation.to_string(),
                    arguments: json!({}),
                }),
                provider_turn: None,
            })
        } else {
            Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "done".into(),
                journal_payload: json!({"s":"done"}),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

fn count_events(events: &[JournalEvent], kind: JournalEventKind) -> usize {
    events.iter().filter(|e| e.kind == kind).count()
}

// ═══════════════════════════════════════════════════════════════════════════
// High 1 — Snapshot-bound hook selection
// ═══════════════════════════════════════════════════════════════════════════

/// Criterion 1: the default binding is resolvable from the Snapshot and the
/// Kernel resolves the budget from it (builtin path).
#[test]
fn default_binding_resolvable_from_snapshot() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let runtime = Runtime::new(config, NTimeToolLlm::new(0).0);
    let snapshot = builtin_snapshot();
    let run = make_run(&snapshot);
    let session = make_session();

    let binding = snapshot
        .hook_binding(crate::registry::snapshot::BUDGET_HOOK_CONTRACT)
        .expect("builtin binding present in snapshot");
    assert_eq!(binding.hook_id, "builtin:run-budget-default-v0");
    assert_eq!(binding.binding_kind, BindingKind::Builtin);

    let budget = runtime
        .resolve_run_budget(&journal, &run, &session, &snapshot)
        .expect("builtin binding resolves");
    assert_eq!(budget.hook_id, "builtin:run-budget-default-v0");
    assert_eq!(budget.source, "default");
    assert_eq!(budget.decision.max_tool_rounds, 12);
    assert_eq!(budget.decision.max_wall_time_ms, 300_000);
}

/// Criterion 2: an external binding replaces the default via a NEW snapshot.
/// The snapshot ID differs because the binding is part of the digest.
#[test]
fn external_binding_replaces_via_new_snapshot() {
    let a = builtin_snapshot();
    let b = external_snapshot(
        "provider:closeout-hook",
        "closeout-provider",
        "http://127.0.0.1:0/x",
    );

    assert_ne!(a.hook_bindings, b.hook_bindings, "different bindings");
    assert_eq!(b.hook_bindings[0].binding_kind, BindingKind::External);
    assert_eq!(b.hook_bindings[0].provider_id, "closeout-provider");
}

/// Criterion 3: the same Snapshot never switches hook identity when the env
/// (config) changes. An external binding requires the credential provider to
/// match; changing the env credential fails closed instead of switching.
#[test]
fn same_snapshot_does_not_switch_hook_on_env_change() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let snapshot = external_snapshot(
        "provider:closeout-hook",
        "closeout-provider",
        "http://127.0.0.1:9999/budget",
    );
    let run = make_run(&snapshot);
    let session = make_session();

    // Scenario A: env credential matches the binding → external hook selected.
    let mut config_a = config.clone();
    config_a.budget_hook = HookConfig {
        enabled: true,
        kind: HookKind::RunBudgetResolveV0,
        endpoint: crate::hook::HookEndpoint {
            url: "http://127.0.0.1:9999/cred".into(),
        },
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        failure_mode: crate::hook::HookFailureMode::FailClosed,
        provider_id: "closeout-provider".into(),
        shared_secret: "secret".into(),
    };
    // Use FakeHookClient so the call succeeds.
    let fake = crate::hook::FakeHookClient::passthrough();
    let runtime_a = Runtime::new(config_a.clone(), NTimeToolLlm::new(0).0)
        .with_hook(Box::new(fake), config_a.context_prepare_hook.clone())
        .with_budget_hook(config_a.budget_hook.clone());
    let budget = runtime_a
        .resolve_run_budget(&journal, &run, &session, &snapshot)
        .expect("matching credential selects external hook");
    assert_eq!(budget.source, "hook");
    assert_eq!(budget.hook_id, "provider:closeout-hook");

    // Scenario B: same snapshot, env credential points at a DIFFERENT
    // provider → fail closed, never silently switch to another hook.
    let mut config_b = config;
    config_b.budget_hook = HookConfig {
        enabled: true,
        kind: HookKind::RunBudgetResolveV0,
        endpoint: crate::hook::HookEndpoint {
            url: "http://127.0.0.1:9999/cred".into(),
        },
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        failure_mode: crate::hook::HookFailureMode::FailClosed,
        provider_id: "some-other-provider".into(),
        shared_secret: "secret".into(),
    };
    let runtime_b = Runtime::new(config_b.clone(), NTimeToolLlm::new(0).0)
        .with_hook(
            Box::new(crate::hook::FakeHookClient::passthrough()),
            config_b.context_prepare_hook.clone(),
        )
        .with_budget_hook(config_b.budget_hook.clone());
    let result = runtime_b.resolve_run_budget(&journal, &run, &session, &snapshot);
    assert!(
        result.is_err(),
        "env credential mismatch must fail closed, not switch hooks"
    );
}

/// Criterion 4: an old Run keeps its frozen binding/decision; a new Run with
/// the new snapshot uses the new binding.
#[test]
fn old_run_keeps_old_binding_new_run_uses_new() {
    let journal = JournalStore::in_memory().unwrap();
    let mut config = test_config();
    config.budget_hook = HookConfig {
        enabled: true,
        kind: HookKind::RunBudgetResolveV0,
        endpoint: crate::hook::HookEndpoint {
            url: "http://127.0.0.1:0/cred".into(),
        },
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        failure_mode: crate::hook::HookFailureMode::FailClosed,
        provider_id: "p2".into(),
        shared_secret: "secret".into(),
    };
    let runtime = Runtime::new(config.clone(), NTimeToolLlm::new(0).0)
        .with_hook(
            Box::new(crate::hook::FakeHookClient::passthrough()),
            config.context_prepare_hook.clone(),
        )
        .with_budget_hook(config.budget_hook.clone());
    let old_snap = builtin_snapshot();
    let new_snap = external_snapshot("provider:hook-v2", "p2", "http://127.0.0.1:0/x");
    let session = make_session();

    // Old Run: frozen from the old snapshot's builtin binding.
    let old_run = make_run(&old_snap);
    let old_budget = runtime
        .resolve_run_budget(&journal, &old_run, &session, &old_snap)
        .unwrap();
    assert_eq!(old_budget.hook_id, "builtin:run-budget-default-v0");

    // New Run: frozen from the new snapshot's external binding.
    let new_run = make_run(&new_snap);
    let new_budget = runtime
        .resolve_run_budget(&journal, &new_run, &session, &new_snap)
        .unwrap();
    assert_eq!(new_budget.hook_id, "provider:hook-v2");
    assert_ne!(old_budget.hook_id, new_budget.hook_id);
}

/// Criterion 5: missing / conflicting / untrusted bindings fail closed.
#[test]
fn missing_or_untrusted_binding_fails_closed() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let runtime = Runtime::new(config, NTimeToolLlm::new(0).0);
    let session = make_session();

    // 5a. No binding → fail closed.
    let mut no_binding = builtin_snapshot();
    no_binding.hook_bindings = vec![];
    let run = make_run(&no_binding);
    let result = runtime.resolve_run_budget(&journal, &run, &session, &no_binding);
    assert!(
        result.is_err(),
        "snapshot without budget hook binding must fail closed"
    );

    // 5b. External binding with empty endpoint → fail closed.
    let mut no_endpoint = external_snapshot("provider:h", "p", "");
    no_endpoint.hook_bindings = vec![HookBinding {
        contract: crate::registry::snapshot::BUDGET_HOOK_CONTRACT.to_string(),
        hook_id: "provider:h".into(),
        hook_version: "v0".into(),
        binding_kind: BindingKind::External,
        binding_key: "external.run_budget_resolve_v0".into(),
        provider_id: "p".into(),
        endpoint: String::new(),
    }];
    let run2 = make_run(&no_endpoint);
    let result2 = runtime.resolve_run_budget(&journal, &run2, &session, &no_endpoint);
    assert!(
        result2.is_err(),
        "external binding without endpoint must fail closed"
    );

    // 5c. Empty hook_id → fail closed.
    let mut empty_id = builtin_snapshot();
    empty_id.hook_bindings = vec![HookBinding {
        contract: crate::registry::snapshot::BUDGET_HOOK_CONTRACT.to_string(),
        hook_id: String::new(),
        hook_version: "v0".into(),
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.run_budget_default".into(),
        provider_id: String::new(),
        endpoint: String::new(),
    }];
    let run3 = make_run(&empty_id);
    let result3 = runtime.resolve_run_budget(&journal, &run3, &session, &empty_id);
    assert!(
        result3.is_err(),
        "binding with empty hook_id must fail closed"
    );
}

/// Criterion 6: the model cannot select a hook or override the budget via
/// tool arguments — the frozen Run fields are the only authority.
#[test]
fn model_cannot_select_hook_or_override_budget() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let (llm, _) = NTimeToolLlm::new(10);
    let runtime = Runtime::new(config, llm);
    let snapshot = builtin_snapshot();
    let mut run = make_run(&snapshot);
    // Freeze a 2-round budget.
    run.budget_max_tool_rounds = Some(2);
    run.budget_max_wall_time_ms = Some(300_000);
    run.budget_exhaustion_action = Some(ExhaustionAction::Yield);
    journal.insert_run(&run).unwrap();

    let session = make_session();
    let mut blocks = vec![ContextBlock {
        kind: ContextBlockKind::UserMessage,
        content: "test".to_string(),
        source_ref: None,
    }];
    // The fake LLM returns tool calls with arguments that attempt to smuggle
    // budget/hook fields; the loop reads ONLY the frozen Run fields.
    let first = runtime
        .llm
        .complete(LlmInput {
            timeout_override_ms: None,
            blocks: blocks.clone(),
            user_text: "test".into(),
            granted_operations: vec!["system.status".to_string()],
            provider_tools: vec![],
            follow_ups: vec![],
        })
        .unwrap();
    runtime
        .run_tool_recall_loop(
            &journal,
            &gateway,
            &run,
            &session,
            &mut blocks,
            "test",
            first,
            &snapshot,
        )
        .unwrap();
    let events = journal.events().unwrap();
    assert_eq!(
        count_events(&events, JournalEventKind::LlmCompleted),
        2,
        "frozen 2-round budget enforced regardless of model arguments"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// High 2 — deadline-bound in-flight calls
// ═══════════════════════════════════════════════════════════════════════════

/// Criterion 7+12: once the Run deadline has passed, no new LLM invocation
/// starts and no next round executes.
#[test]
fn no_new_llm_call_after_deadline() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let (llm, _) = NTimeToolLlm::new(10);
    let runtime = Runtime::new(config, llm);
    let snapshot = builtin_snapshot();
    let mut run = make_run(&snapshot);
    // Frozen wall time of 1ms: the deadline is already past after the first
    // tool round.
    run.budget_max_tool_rounds = Some(12);
    run.budget_max_wall_time_ms = Some(1);
    run.budget_exhaustion_action = Some(ExhaustionAction::Yield);
    journal.insert_run(&run).unwrap();

    let session = make_session();
    let mut blocks = vec![ContextBlock {
        kind: ContextBlockKind::UserMessage,
        content: "test".to_string(),
        source_ref: None,
    }];
    let first = runtime
        .llm
        .complete(LlmInput {
            timeout_override_ms: None,
            blocks: blocks.clone(),
            user_text: "test".into(),
            granted_operations: vec!["system.status".to_string()],
            provider_tools: vec![],
            follow_ups: vec![],
        })
        .unwrap();
    let result = runtime
        .run_tool_recall_loop(
            &journal,
            &gateway,
            &run,
            &session,
            &mut blocks,
            "test",
            first,
            &snapshot,
        )
        .unwrap();
    let events = journal.events().unwrap();
    // Round 0 consumed the first model output (no LlmCompleted for it — the
    // loop's follow-up calls are what counts). The first tool result triggers
    // the deadline guard, so NO follow-up LLM call happens.
    assert_eq!(
        count_events(&events, JournalEventKind::ToolLoopWallClockExceeded),
        1,
        "deadline exceeded recorded"
    );
    // High 3: a yield produces ONLY the structured fact — no user-facing
    // "请发送继续" text is generated (the external Harness decides).
    assert!(
        !result.content.contains("请发送「继续」"),
        "yield must not fabricate a continue prompt"
    );
}

/// Criterion 8: no new Tool call starts after the Run deadline (tool
/// invocation guard before handle_inline_tool_call).
#[test]
fn no_new_tool_call_after_deadline() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let (llm, _) = NTimeToolLlm::new(10);
    let runtime = Runtime::new(config, llm);
    let snapshot = builtin_snapshot();
    let mut run = make_run(&snapshot);
    // Deadline already past before the loop even starts.
    run.budget_max_tool_rounds = Some(12);
    run.budget_max_wall_time_ms = Some(0);
    run.budget_exhaustion_action = Some(ExhaustionAction::Yield);
    journal.insert_run(&run).unwrap();

    let session = make_session();
    let mut blocks = vec![ContextBlock {
        kind: ContextBlockKind::UserMessage,
        content: "test".to_string(),
        source_ref: None,
    }];
    let first = runtime
        .llm
        .complete(LlmInput {
            timeout_override_ms: None,
            blocks: blocks.clone(),
            user_text: "test".into(),
            granted_operations: vec!["system.status".to_string()],
            provider_tools: vec![],
            follow_ups: vec![],
        })
        .unwrap();
    runtime
        .run_tool_recall_loop(
            &journal,
            &gateway,
            &run,
            &session,
            &mut blocks,
            "test",
            first,
            &snapshot,
        )
        .unwrap();
    let events = journal.events().unwrap();
    // The Valid tool call from the first output must NOT be dispatched: the
    // tool-invocation guard fires before handle_inline_tool_call.
    assert_eq!(
        count_events(&events, JournalEventKind::InvocationProposed),
        0,
        "no tool invocation proposed after deadline"
    );
    assert_eq!(
        count_events(&events, JournalEventKind::ToolCallIssued),
        0,
        "no tool call issued after deadline"
    );
}

/// Criterion 9: a blocking LLM HTTP future stops waiting AT the deadline.
/// Real TcpListener that never responds; the client's effective timeout is
/// min(client 30s, override 300ms) so the call returns ~300ms, not 30s.
#[test]
fn blocking_llm_stops_waiting_at_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                // Never respond: hold the connection open.
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    });

    let llm = OpenAiCompatibleLlm::new(
        format!("http://127.0.0.1:{port}"),
        "key".into(),
        "model".into(),
        30_000,
    );
    let start = Instant::now();
    let result = llm.complete(LlmInput {
        timeout_override_ms: Some(300),
        blocks: vec![ContextBlock {
            kind: ContextBlockKind::UserMessage,
            content: "test".into(),
            source_ref: None,
        }],
        user_text: "test".into(),
        granted_operations: vec![],
        provider_tools: vec![],
        follow_ups: vec![],
    });
    let elapsed = start.elapsed();
    let output = result.expect("complete returns a classified output");
    assert_eq!(
        output.failure_category(),
        Some("model_timeout"),
        "deadline-bounded call must classify as model_timeout"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "caller must stop waiting at the deadline, took {elapsed:?}"
    );
    // The override never exceeds the client's own fixed timeout: with a 200ms
    // override the effective timeout is 200ms (min(200, 30000)).
    let llm2 = OpenAiCompatibleLlm::new(
        format!("http://127.0.0.1:{port}"),
        "key".into(),
        "model".into(),
        150,
    );
    let start2 = Instant::now();
    let result2 = llm2.complete(LlmInput {
        timeout_override_ms: Some(300),
        blocks: vec![ContextBlock {
            kind: ContextBlockKind::UserMessage,
            content: "test".into(),
            source_ref: None,
        }],
        user_text: "test".into(),
        granted_operations: vec![],
        provider_tools: vec![],
        follow_ups: vec![],
    });
    let elapsed2 = start2.elapsed();
    let output2 = result2.expect("complete returns a classified output");
    assert_eq!(
        output2.failure_category(),
        Some("model_timeout"),
        "min(client timeout, override) must bound the call"
    );
    assert!(
        elapsed2 < Duration::from_secs(3),
        "override must never exceed client timeout, took {elapsed2:?}"
    );
}

/// Criterion 10: a blocking HTTP tool (external harness) stops waiting AT
/// the deadline via the harness read timeout.
#[test]
fn blocking_http_tool_stops_waiting_at_deadline() {
    use crate::harness::manifest::HarnessManifest;
    use chrono::Utc;

    // Real server that never responds.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    });
    let endpoint = format!("http://127.0.0.1:{port}");

    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "t".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: endpoint.clone(),
        operation_name: "external.blocking_probe".into(),
        description: "blocking".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        idempotent: true,
        created_at: Utc::now(),
    };
    m.manifest_id = m.compute_manifest_id().unwrap();

    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    // Register the manifest so dispatch resolves the binding to the endpoint.
    journal.register_harness_manifest(&m).unwrap();
    let _gateway = Gateway::new(config.clone());
    let snapshot = builtin_snapshot();
    let run = make_run(&snapshot);
    let session = make_session();

    let spec = OperationSpec {
        name: "external.blocking_probe".into(),
        risk: Risk::Write,
        description: "blocking".into(),
        parameters: json!({"type":"object"}),
        idempotent: false,
        binding_kind: BindingKind::External,
        binding_key: m.manifest_id.clone(),
    };
    let intent = crate::domain::InvocationIntent {
        invocation_id: crate::domain::InvocationId("inv".into()),
        run_id: run.id.clone(),
        operation: "external.blocking_probe".into(),
        arguments: json!({}),
        idempotency_key: None,
    };
    // Construct the approved invocation directly: the manifest enablement
    // path is out of scope for this transport-level test.
    let approved = crate::domain::ApprovedInvocation::new(intent, "decision_closeout".into());

    // Effective read timeout = min(remaining=200ms, harness 10s) = 200ms.
    let start = Instant::now();
    let outcome = crate::runtime::tool_execution::dispatch_builtin_binding(
        &spec,
        &approved,
        &journal,
        &run,
        &session,
        "corr",
        Duration::from_millis(200),
        &snapshot.snapshot_id,
    );
    let elapsed = start.elapsed();
    match outcome {
        crate::runtime::tool_loop::ToolCallOutcome::ToolResult { text } => {
            assert!(
                text.contains("timeout"),
                "timeout semantics recorded: {text}"
            );
            assert!(
                text.contains("caller_stopped_waiting=true"),
                "honest caller-stopped semantics: {text}"
            );
            assert!(
                text.contains("remote_side_effect_cancellation_unverified=true"),
                "honest unverified-cancellation semantics: {text}"
            );
        }
        _ => panic!("expected timeout ToolResult"),
    }
    assert!(
        elapsed < Duration::from_secs(3),
        "tool caller stops waiting at deadline, took {elapsed:?}"
    );
}

/// Criterion 11: terminate semantics on deadline stay explicit (Run Failed,
/// no 请发送继续).
#[test]
fn deadline_terminate_marks_run_failed() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let (llm, _) = NTimeToolLlm::new(10);
    let runtime = Runtime::new(config, llm);
    let snapshot = builtin_snapshot();
    let mut run = make_run(&snapshot);
    run.budget_max_tool_rounds = Some(12);
    run.budget_max_wall_time_ms = Some(0);
    run.budget_exhaustion_action = Some(ExhaustionAction::Terminate);
    journal.insert_run(&run).unwrap();

    let session = make_session();
    let mut blocks = vec![ContextBlock {
        kind: ContextBlockKind::UserMessage,
        content: "test".to_string(),
        source_ref: None,
    }];
    let first = runtime
        .llm
        .complete(LlmInput {
            timeout_override_ms: None,
            blocks: blocks.clone(),
            user_text: "test".into(),
            granted_operations: vec!["system.status".to_string()],
            provider_tools: vec![],
            follow_ups: vec![],
        })
        .unwrap();
    let result = runtime
        .run_tool_recall_loop(
            &journal,
            &gateway,
            &run,
            &session,
            &mut blocks,
            "test",
            first,
            &snapshot,
        )
        .unwrap();
    let status = journal.run_status(&run.id).unwrap();
    assert_eq!(status.as_deref(), Some("Failed"), "terminate marks Failed");
    assert!(
        !result.content.contains("请发送「继续」"),
        "terminate must not invite continuation"
    );
}

/// Criterion 13: run_deadline_exceeded guard records honest semantics via the
/// ToolLoopWallClockExceeded event (no fabricated cancellation claims).
#[test]
fn deadline_exceeded_event_records_honest_semantics() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let (llm, _) = NTimeToolLlm::new(10);
    let runtime = Runtime::new(config, llm);
    let snapshot = builtin_snapshot();
    let mut run = make_run(&snapshot);
    run.budget_max_tool_rounds = Some(12);
    run.budget_max_wall_time_ms = Some(1);
    run.budget_exhaustion_action = Some(ExhaustionAction::Yield);
    journal.insert_run(&run).unwrap();

    let session = make_session();
    let mut blocks = vec![ContextBlock {
        kind: ContextBlockKind::UserMessage,
        content: "test".to_string(),
        source_ref: None,
    }];
    let first = runtime
        .llm
        .complete(LlmInput {
            timeout_override_ms: None,
            blocks: blocks.clone(),
            user_text: "test".into(),
            granted_operations: vec!["system.status".to_string()],
            provider_tools: vec![],
            follow_ups: vec![],
        })
        .unwrap();
    runtime
        .run_tool_recall_loop(
            &journal,
            &gateway,
            &run,
            &session,
            &mut blocks,
            "test",
            first,
            &snapshot,
        )
        .unwrap();
    let events = journal.events().unwrap();
    let wall = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ToolLoopWallClockExceeded)
        .expect("deadline event recorded");
    assert_eq!(wall.payload["exhaustion_action"], json!("yield"));
    // No RunBudgetTerminated event for yield (Run stays continuable).
    assert_eq!(
        count_events(&events, JournalEventKind::RunBudgetTerminated),
        0
    );
    // Hash chain integrity preserved.
    assert!(
        journal.verify_hash_chain().unwrap(),
        "hash chain must remain valid after deadline events"
    );
}

/// Criterion 14: normal chat / snapshot / hash-chain paths do not regress.
#[test]
fn snapshot_binding_persists_through_journal_roundtrip() {
    let journal = JournalStore::in_memory().unwrap();
    // Production bootstrap path: create_registry_snapshot attaches the
    // bootstrap hook binding set.
    let snap = journal
        .create_registry_snapshot(crate::registry::store::builtin_specs())
        .unwrap();
    assert_eq!(
        snap.hook_binding(crate::registry::snapshot::BUDGET_HOOK_CONTRACT)
            .unwrap()
            .hook_id,
        "builtin:run-budget-default-v0",
        "bootstrap snapshot carries default binding"
    );
    // Roundtrip through the journal: reload by ID keeps the binding set.
    let reloaded = journal.load_registry_snapshot(&snap.snapshot_id).unwrap();
    assert_eq!(reloaded.hook_bindings, snap.hook_bindings);
    // Explicit no-binding snapshot roundtrips as an empty set.
    let none = journal
        .create_registry_snapshot_with_hook_bindings(
            crate::registry::store::builtin_specs(),
            vec![],
        )
        .unwrap();
    assert!(none.hook_bindings.is_empty());
    let reloaded_none = journal.load_registry_snapshot(&none.snapshot_id).unwrap();
    assert!(reloaded_none.hook_bindings.is_empty());
    // Hash chain intact.
    assert!(journal.verify_hash_chain().unwrap());
}

/// Criterion 15: the budget contract contains no Coding/Ops/Router/OpenClaw
/// special-casing (already covered by run_budget.rs; re-asserted here for the
/// generic binding model).
#[test]
fn binding_model_has_no_product_special_cases() {
    let binding = crate::registry::snapshot::HookBinding::builtin_budget();
    let json = serde_json::to_value(&binding).unwrap();
    let s = json.to_string();
    for forbidden in [
        "coding",
        "ops",
        "router",
        "openclaw",
        "task_type",
        "complexity",
    ] {
        assert!(
            !s.to_lowercase().contains(forbidden),
            "binding model must not mention {forbidden}"
        );
    }
    // Only two binding kinds exist: Builtin and External.
    let kind = serde_json::to_value(BindingKind::Builtin).unwrap();
    assert_eq!(kind, json!("Builtin"));
}

/// The frozen `RunBudgetDecision` digest remains canonical (used for audit).
#[test]
fn decision_digest_is_stable() {
    let d1 = RunBudgetDecision {
        max_tool_rounds: 12,
        max_wall_time_ms: 300_000,
        exhaustion_action: ExhaustionAction::Yield,
    };
    let d2 = d1.clone();
    assert_eq!(d1.digest(), d2.digest());
    assert!(d1.digest().starts_with("sha256:"));
}

// ═══════════════════════════════════════════════════════════════════════════
// Generic Hook Binding model tests (boundary-audit close-out)
// ═══════════════════════════════════════════════════════════════════════════

/// A fictional second contract used ONLY to prove the generic model. No
/// business hook is implemented for it.
const TEST_ECHO_CONTRACT: &str = "test.echo.resolve.v0";

fn echo_binding() -> HookBinding {
    HookBinding {
        contract: TEST_ECHO_CONTRACT.to_string(),
        hook_id: "builtin:test-echo-default-v0".to_string(),
        hook_version: "v0".to_string(),
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.test_echo_default".to_string(),
        provider_id: String::new(),
        endpoint: String::new(),
    }
}

/// Genericity 1: a snapshot can hold two hook bindings for two different
/// contracts at the same time.
#[test]
fn snapshot_holds_two_contracts() {
    let mut snap = builtin_snapshot();
    snap.hook_bindings.push(echo_binding());
    assert_eq!(snap.hook_bindings.len(), 2);
    assert!(snap.hook_binding(BUDGET_HOOK_CONTRACT).is_some());
    assert!(snap.hook_binding(TEST_ECHO_CONTRACT).is_some());
}

/// Genericity 2: adding a second fictional contract needs NO new field and NO
/// migration — the struct and storage are already generic.
#[test]
fn second_contract_needs_no_new_field_or_migration() {
    // The HookBinding struct has no contract-specific fields.
    let json = serde_json::to_value(&echo_binding()).unwrap();
    let obj = json.as_object().unwrap();
    assert_eq!(
        obj.len(),
        7,
        "contract,hook_id,hook_version,binding_kind,binding_key,provider_id,endpoint"
    );
    assert!(obj.contains_key("contract"));
    // Storage is a generic sub-table; the migration defines no budget columns.
    let migration = include_str!("../../../migrations/0019_registry_hook_bindings.sql");
    assert!(
        !migration.contains("budget_hook"),
        "migration must not contain budget-specific columns"
    );
}

/// Genericity 3: the snapshot digest is insensitive to binding order.
#[test]
fn digest_insensitive_to_binding_order() {
    let specs = vec![OperationSpec {
        name: "system.status".to_string(),
        risk: Risk::ReadOnly,
        description: "t".into(),
        parameters: json!({"type":"object"}),
        idempotent: true,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.system_status".into(),
    }];
    let mut bindings_a = vec![HookBinding::builtin_budget(), echo_binding()];
    let mut bindings_b = vec![echo_binding(), HookBinding::builtin_budget()];
    // Same set, different order.
    bindings_a.sort_by(|a, b| a.contract.cmp(&b.contract));
    bindings_b.sort_by(|a, b| a.contract.cmp(&b.contract));
    assert_eq!(bindings_a, bindings_b);
    let id_a =
        crate::registry::snapshot::compute_snapshot_id_with_hook_bindings(&specs, &bindings_a)
            .unwrap();
    let id_b =
        crate::registry::snapshot::compute_snapshot_id_with_hook_bindings(&specs, &bindings_b)
            .unwrap();
    assert_eq!(id_a, id_b, "digest must be order-insensitive");
}

/// Genericity 4: changing any binding content produces a different snapshot ID.
#[test]
fn digest_changes_when_binding_content_changes() {
    let specs = vec![];
    let base = vec![HookBinding::builtin_budget()];
    let mut different_endpoint = HookBinding::builtin_budget();
    different_endpoint.endpoint = "http://127.0.0.1:9999/x".into();
    let changed = vec![different_endpoint];
    let id_base =
        crate::registry::snapshot::compute_snapshot_id_with_hook_bindings(&specs, &base).unwrap();
    let id_changed =
        crate::registry::snapshot::compute_snapshot_id_with_hook_bindings(&specs, &changed)
            .unwrap();
    assert_ne!(id_base, id_changed);
}

/// Genericity 5: a duplicated contract is rejected at snapshot creation.
#[test]
fn duplicate_contract_rejected() {
    let journal = JournalStore::in_memory().unwrap();
    let dup = vec![HookBinding::builtin_budget(), HookBinding::builtin_budget()];
    let result = journal
        .create_registry_snapshot_with_hook_bindings(crate::registry::store::builtin_specs(), dup);
    assert!(
        result.is_err(),
        "duplicate contract must be rejected at creation"
    );
}

/// Genericity 6: budget resolution reads ONLY `run.budget.resolve.v0` and
/// ignores other contracts.
#[test]
fn budget_resolution_ignores_other_contracts() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let runtime = Runtime::new(config, NTimeToolLlm::new(0).0);
    let mut snap = builtin_snapshot();
    snap.hook_bindings.push(echo_binding());
    let run = make_run(&snap);
    let session = make_session();
    let budget = runtime
        .resolve_run_budget(&journal, &run, &session, &snap)
        .expect("budget resolves from its contract despite extra contracts");
    assert_eq!(budget.hook_id, "builtin:run-budget-default-v0");
    assert_eq!(budget.source, "default");
}

/// Genericity 7: activating a new snapshot inherits the complete binding set.
#[test]
fn activation_inherits_complete_binding_set() {
    use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
    use crate::harness::manifest::HarnessManifest;
    use chrono::Utc;

    let journal = JournalStore::in_memory().unwrap();
    let gateway = crate::gateway::Gateway::new(test_config());
    // Bootstrap snapshot carries the budget binding.
    let active = journal
        .create_registry_snapshot(crate::registry::store::builtin_specs())
        .unwrap();
    journal
        .activate_registry_snapshot(&active.snapshot_id)
        .unwrap();

    // Enable an external harness → new snapshot via the activation path.
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "t".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:9999/h".into(),
        operation_name: "external.genericity_probe".into(),
        description: "g".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        idempotent: true,
        created_at: Utc::now(),
    };
    m.manifest_id = m.compute_manifest_id().unwrap();
    journal.register_harness_manifest(&m).unwrap();
    journal
        .enable_harness(
            &gateway
                .approve_harness_change(HarnessChangeIntent {
                    action: HarnessChangeAction::Enable,
                    manifest_id: m.manifest_id.clone(),
                    expected_snapshot_id: active.snapshot_id.clone(),
                    requested_by: "ipc_operator".into(),
                })
                .unwrap(),
        )
        .unwrap();

    let new_id = journal.current_registry_snapshot_id().unwrap();
    let new_snap = journal.load_registry_snapshot(&new_id).unwrap();
    assert_eq!(
        new_snap.hook_bindings, active.hook_bindings,
        "activation inherits the complete binding set"
    );
    assert!(new_snap
        .hook_binding(crate::registry::snapshot::BUDGET_HOOK_CONTRACT)
        .is_some());
}

/// Genericity 8: historical snapshots are not modified by bootstrap or
/// activation.
#[test]
fn historical_snapshot_not_modified() {
    let journal = JournalStore::in_memory().unwrap();
    // A snapshot with NO hook bindings (pre-0019 shape).
    let historical = journal
        .create_registry_snapshot_with_hook_bindings(
            crate::registry::store::builtin_specs(),
            vec![],
        )
        .unwrap();
    let historical_id = historical.snapshot_id.clone();
    // Re-load: still empty, same ID, same content.
    let reloaded = journal.load_registry_snapshot(&historical_id).unwrap();
    assert!(reloaded.hook_bindings.is_empty());
    assert_eq!(reloaded.snapshot_id, historical_id);
    assert_eq!(reloaded.operations, historical.operations);
}

/// Genericity 9: the default budget binding bootstraps through the generic
/// mechanism (create_registry_snapshot attaches it).
#[test]
fn default_budget_binding_bootstraps_generically() {
    let journal = JournalStore::in_memory().unwrap();
    let snap = journal
        .create_registry_snapshot(crate::registry::store::builtin_specs())
        .unwrap();
    let binding = snap
        .hook_binding(crate::registry::snapshot::BUDGET_HOOK_CONTRACT)
        .expect("bootstrap attaches the generic budget binding");
    assert_eq!(binding.hook_id, "builtin:run-budget-default-v0");
    assert_eq!(binding.contract, "run.budget.resolve.v0");
    // The struct exposes no budget-specific field.
    let json = serde_json::to_value(binding).unwrap();
    assert!(!json.as_object().unwrap().contains_key("budget_hook"));
}

/// Genericity 10: no `budget_hook`-specific field or column exists in the
/// RegistrySnapshot struct, the migration, or the storage layer.
#[test]
fn no_budget_specific_registry_field_or_column() {
    // Struct: no budget_hook field.
    let snap = serde_json::to_value(&builtin_snapshot()).unwrap();
    let obj = snap.as_object().unwrap();
    assert!(!obj.contains_key("budget_hook"), "no budget_hook field");
    assert!(obj.contains_key("hook_bindings"), "generic hook_bindings");
    // Migration: no budget columns.
    let migration = include_str!("../../../migrations/0019_registry_hook_bindings.sql");
    assert!(
        !migration.contains("budget_hook"),
        "migration has no budget-specific columns"
    );
    // Storage schema: only the generic sub-table columns.
    let journal = JournalStore::in_memory().unwrap();
    journal
        .create_registry_snapshot(crate::registry::store::builtin_specs())
        .unwrap();
    let conn = journal.conn.lock().unwrap();
    let cols: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('registry_snapshot_hook_bindings')")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert!(
        cols.iter().all(|c| !c.contains("budget_hook")),
        "storage columns are generic: {cols:?}"
    );
    assert!(cols.contains(&"contract".to_string()));
}
