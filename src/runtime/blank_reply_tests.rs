use super::super::Runtime;
use super::tool_loop_tests::test_config;
use crate::domain::JournalEventKind;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use serde_json::json;

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
    assert!(runtime.deliver(&journal, &gateway, event).is_err());
    let failed = journal
        .events()
        .unwrap()
        .into_iter()
        .find(|event| event.kind == JournalEventKind::RunFailed)
        .unwrap();
    assert_eq!(
        journal
            .run_status(failed.run_id.as_ref().unwrap())
            .unwrap()
            .as_deref(),
        Some("Failed")
    );
    assert_eq!(failed.payload["error_category"], "tool_followup_llm_failed");
    assert!(!serde_json::to_string(&failed)
        .unwrap()
        .contains("provider details"));
}
