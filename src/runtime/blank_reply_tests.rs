use super::super::Runtime;
use super::tool_loop_tests::test_config;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmFollowUp, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use serde_json::{json, Value};
use std::sync::atomic::AtomicUsize;

struct WhitespaceLlm {
    first_tool: bool,
    calls: std::sync::atomic::AtomicUsize,
}

impl LlmClient for WhitespaceLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tool_call = if self.first_tool && call == 0 {
            ToolCallResult::Valid(ToolCall {
                id: "provider-id-digest".into(),
                operation: "system.status".into(),
                arguments: json!({}),
            })
        } else {
            ToolCallResult::Absent
        };
        Ok(LlmOutput {
            provider: "test".into(),
            model: "whitespace".into(),
            content: "  \n".into(),
            journal_payload: json!({"round": call}),
            tool_call,
            provider_turn: None,
        })
    }
}

fn assert_whitespace_reply_is_guarded(first_tool: bool) {
    let mut config = test_config();
    config.extra_allowed_operations = vec!["system.status".into()];
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(
        config,
        WhitespaceLlm {
            first_tool,
            calls: std::sync::atomic::AtomicUsize::new(0),
        },
    );
    let event = gateway
        .validate_ingress(&journal, gateway.cli_ingress("hello".into()).unwrap())
        .unwrap();
    let outcome = runtime.deliver(&journal, &gateway, event).unwrap();
    assert!(!outcome.output.trim().is_empty());
    assert_ne!(
        journal.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Running")
    );
    assert_eq!(
        journal
            .events()
            .unwrap()
            .iter()
            .filter(|event| event.kind == JournalEventKind::OutboxQueued)
            .count(),
        1
    );
}
#[test]
fn first_round_whitespace_reply_is_not_enqueued_blank() {
    assert_whitespace_reply_is_guarded(false);
}
#[test]
fn post_tool_whitespace_reply_is_not_enqueued_blank() {
    assert_whitespace_reply_is_guarded(true);
}

struct FailingRecallLlm(std::sync::Mutex<usize>);

impl LlmClient for FailingRecallLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        let mut round = self.0.lock().unwrap();
        let current = *round;
        *round += 1;
        Ok(LlmOutput {
            provider: "test".into(),
            model: "failing-recall".into(),
            content: if current == 0 {
                "recalling"
            } else {
                "recovered"
            }
            .into(),
            journal_payload: json!({"round": current}),
            tool_call: if current == 0 {
                ToolCallResult::Valid(ToolCall {
                    id: crate::llm::tool_call_id_hash("failing_recall"),
                    operation: "session.recall_recent".into(),
                    arguments: json!({}),
                })
            } else {
                ToolCallResult::Absent
            },
            provider_turn: None,
        })
    }
}
#[test]
fn recall_query_failure_closes_receipt_and_run_without_leaks() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    journal.set_recall_failure_for_test(true);
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, FailingRecallLlm(std::sync::Mutex::new(0)));
    let event = gateway
        .validate_ingress(
            &journal,
            gateway.cli_ingress("recall something".into()).unwrap(),
        )
        .unwrap();
    let outcome = runtime.deliver(&journal, &gateway, event).unwrap();
    assert_ne!(
        journal.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Running")
    );
    let events = journal.events().unwrap();
    let receipts: Vec<_> = events
        .iter()
        .filter(|event| event.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].payload["status"], "Failed");
    assert_eq!(
        receipts[0].payload["output"]["error_category"],
        "harness_failed"
    );
    assert!(receipts[0].payload["output"].get("messages").is_none());
    let audit = serde_json::to_string(&events).unwrap();
    for forbidden in ["sqlite", "journal_events", "recall_query_failed"] {
        assert!(!audit.contains(forbidden), "leaked {forbidden}");
    }
}

struct FollowupFailureLlm(std::sync::atomic::AtomicUsize);

impl LlmClient for FollowupFailureLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0 {
            anyhow::bail!("provider details must not escape");
        }
        Ok(LlmOutput {
            provider: "test".into(),
            model: "followup-failure".into(),
            content: "calling".into(),
            journal_payload: json!({"round": 0}),
            tool_call: ToolCallResult::Valid(ToolCall {
                id: "provider-id-digest".into(),
                operation: "system.status".into(),
                arguments: json!({}),
            }),
            provider_turn: None,
        })
    }
}
#[test]
fn failed_followup_llm_marks_the_accurate_run_failed() {
    let mut config = test_config();
    config.extra_allowed_operations = vec!["system.status".into()];
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(
        config,
        FollowupFailureLlm(std::sync::atomic::AtomicUsize::new(0)),
    );
    let event = gateway
        .validate_ingress(&journal, gateway.cli_ingress("time".into()).unwrap())
        .unwrap();
    // deliver() now returns Ok with the failure message, not Err.
    let outcome = runtime.deliver(&journal, &gateway, event).unwrap();
    let events = journal.events().unwrap();

    // Run status must be Failed.
    assert_eq!(
        journal.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Failed")
    );

    // Exactly one RunFailed event with the correct category.
    let failed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::RunFailed)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].payload["error_category"],
        "tool_followup_llm_failed"
    );

    // InvocationProposed: 1 for tool call + 1 for failure reply = 2.
    let props: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::InvocationProposed)
        .collect();
    assert_eq!(props.len(), 2, "InvocationProposed: tool + failure");

    // InvocationApproved: 1 for tool call + 1 for failure reply = 2.
    let approved: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::InvocationApproved)
        .collect();
    assert_eq!(approved.len(), 2);

    // Exactly one OutboxQueued for the failure reply (tool uses sync dispatch).
    let oq: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .collect();
    assert_eq!(oq.len(), 1, "failure reply must be enqueued to outbox");

    // No RunCompleted (run is Failed, not Completed).
    assert!(events
        .iter()
        .all(|e| e.kind != JournalEventKind::RunCompleted));

    // The output is the static Chinese failure message, no provider details.
    assert!(
        outcome.output.contains("模型生成后续回复时失败了")
            || outcome.output.contains("工具执行结果已记录")
    );
    assert!(!outcome.output.contains("provider details"));
    assert!(!outcome.output.contains("tool_followup_llm_failed"));

    // Journal hash chain is valid.
    assert!(journal.verify_hash_chain().unwrap());
}
/// LLM that fails on the very first call (no tool execution).
struct InitialFailureLlm;

impl LlmClient for InitialFailureLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        anyhow::bail!("simulated initial LLM failure, no provider details");
    }
}
#[test]
fn initial_llm_failure_still_replies() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, InitialFailureLlm);
    let event = gateway
        .validate_ingress(&journal, gateway.cli_ingress("fail early".into()).unwrap())
        .unwrap();
    let outcome = runtime.deliver(&journal, &gateway, event).unwrap();
    let events = journal.events().unwrap();

    assert_eq!(
        journal.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Failed")
    );

    let failed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::RunFailed)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].payload["error_category"], "initial_llm_failed");

    // There should be exactly one reply OutboxQueued.
    let oq: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .collect();
    assert_eq!(oq.len(), 1, "initial failure must enqueue a reply");

    // The output must be the static failure message, not the raw error.
    assert!(
        outcome.output.contains("模型暂时不可用"),
        "static failure message: {}",
        outcome.output
    );
    assert!(!outcome.output.contains("simulated"));
    assert!(!outcome.output.contains("provider"));

    // No LlmCompleted (first call failed before journal record).
    assert!(events
        .iter()
        .all(|e| e.kind != JournalEventKind::LlmCompleted));
}
/// LLM that returns a tool call whose execution fails, then follow-up fails.
struct ToolFailsThenFollowupFailsLlm(AtomicUsize);

impl LlmClient for ToolFailsThenFollowupFailsLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        let call = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match call {
            0 => Ok(LlmOutput {
                provider: "test".into(),
                model: "t".into(),
                content: "call forbidden tool".into(),
                journal_payload: json!({"round": 0}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "forbidden".into(),
                    operation: "shell.exec".into(),
                    arguments: json!({}),
                }),
                provider_turn: None,
            }),
            _ => anyhow::bail!("follow-up LLM failure"),
        }
    }
}
#[test]
fn tool_failure_then_llm_failure_still_replies() {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, ToolFailsThenFollowupFailsLlm(AtomicUsize::new(0)));
    let event = gateway
        .validate_ingress(&journal, gateway.cli_ingress("tool fail".into()).unwrap())
        .unwrap();
    let outcome = runtime.deliver(&journal, &gateway, event).unwrap();
    let events = journal.events().unwrap();

    assert_eq!(
        journal.run_status(&outcome.run_id).unwrap().as_deref(),
        Some("Failed")
    );

    // ToolCallRejected for the forbidden operation.
    let rejected: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::ToolCallRejected)
        .collect();
    assert_eq!(rejected.len(), 1);

    // RunFailed for the follow-up LLM failure.
    let failed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::RunFailed)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].payload["error_category"],
        "tool_followup_llm_failed"
    );

    // Reply outbox entry.
    let oq: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .collect();
    assert_eq!(oq.len(), 1, "failure reply must be enqueued");

    // Static failure message, no internals.
    assert!(outcome.output.contains("模型生成后续回复时失败了"));
    assert!(!outcome.output.contains("shell.exec"));
}
/// Verify direct duplicate calls to reply_with_failure produce exactly one
/// outbox entry thanks to the stable idempotency key.
#[test]
fn duplicate_failure_reply_not_enqueued_twice() {
    let mut config = test_config();
    config.extra_allowed_operations = vec!["system.status".into()];
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let snapshot = crate::registry::snapshot::test_snapshot();
    let run = Run {
        id: RunId::new(),
        session_id: SessionId("s_dup".into()),
        agent_id: AgentId("main".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:local".into()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![
                CapabilityGrant {
                    operation: "stdout.send_text".into(),
                    scope: "current_session".into(),
                },
                CapabilityGrant {
                    operation: "system.status".into(),
                    scope: "current_session".into(),
                },
            ],
            requester_id: Some("cli:local".into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: String::new(),
    };
    journal.insert_run(&run).unwrap();
    let session = Session {
        id: SessionId("s_dup".into()),
        agent_id: AgentId("main".into()),
        channel: ChannelKind::Cli,
        conversation_key: "local".into(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: chrono::Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    };
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);

    // First call.
    let r1 = runtime.reply_with_failure(
        &journal,
        &gateway,
        &snapshot,
        &run,
        &session,
        None,
        None,
        "failure msg",
    );
    assert!(r1.is_ok());
    let events_after_1st = journal.events().unwrap();
    let oq1: Vec<_> = events_after_1st
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .collect();
    assert_eq!(oq1.len(), 1, "first call creates one outbox entry");

    // Second call with same run — idempotency key should prevent duplication.
    let r2 = runtime.reply_with_failure(
        &journal,
        &gateway,
        &snapshot,
        &run,
        &session,
        None,
        None,
        "failure msg",
    );
    assert!(r2.is_ok());
    let events_after_2nd = journal.events().unwrap();
    let oq2: Vec<_> = events_after_2nd
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .collect();
    assert_eq!(
        oq2.len(),
        1,
        "second call does NOT add a second outbox entry"
    );

    // The idempotency key prevents duplicate outbox.
    assert!(oq2.len() == 1, "at most one outbox dispatch");
}

/// Error injection: queue_outbox_dispatch failure after RunFailed.
#[test]
fn outbox_failure_after_runfailed_returns_err() {
    let mut config = test_config();
    config.extra_allowed_operations = vec!["system.status".into()];
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, FollowupFailureLlm(AtomicUsize::new(0)));
    // Drop outbox_dispatches table so queue_outbox_dispatch fails.
    journal
        .execute_sql_for_test("DROP TABLE outbox_dispatches")
        .unwrap();
    let event = gateway
        .validate_ingress(&journal, gateway.cli_ingress("x".into()).unwrap())
        .unwrap();
    let result = runtime.deliver(&journal, &gateway, event);
    assert!(
        result.is_err(),
        "deliver must return Err when outbox enqueue fails"
    );
    // RunFailed should still be recorded.
    let events = journal.events().unwrap();
    let failed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::RunFailed)
        .collect();
    assert_eq!(failed.len(), 1, "RunFailed recorded despite outbox failure");
    // No OutboxQueued.
    let oq: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxQueued)
        .collect();
    assert_eq!(oq.len(), 0, "no outbox entry on failure");
    // No RunCompleted.
    assert!(
        events
            .iter()
            .all(|e| e.kind != JournalEventKind::RunCompleted),
        "no RunCompleted"
    );
}