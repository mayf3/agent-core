//! Recall security: CapturingRecallLlm with real ProviderToolTurn.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{
    EndpointChoice, LlmClient, LlmInput, LlmOutput, ProviderToolTurn, ToolCall, ToolCallResult,
};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

// =========================================================================
// CapturingRecallLlm — real ProviderToolTurn in round 1
// =========================================================================

#[allow(dead_code)]
pub(crate) struct CapturingRecallLlm {
    pub inputs: Arc<Mutex<Vec<LlmInput>>>,
    round: Mutex<usize>,
}

impl CapturingRecallLlm {
    pub fn new() -> Self {
        Self {
            inputs: Arc::new(Mutex::new(Vec::new())),
            round: Mutex::new(0),
        }
    }
}

impl LlmClient for CapturingRecallLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        self.inputs.lock().unwrap().push(input);
        let mut round = self.round.lock().unwrap();
        *round += 1;
        if *round == 1 {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "test".into(),
                content: String::new(),
                journal_payload: json!({}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "recall_call_1".into(),
                    operation: "session.recall_recent".into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(ProviderToolTurn {
                    endpoint: EndpointChoice::Primary,
                    provider_tool_call_id: "recall_call_1".into(),
                    wire_name: "session.recall_recent".into(),
                    canonical_operation: "session.recall_recent".into(),
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "test".into(),
                content: "final assistant reply with recall data".into(),
                journal_payload: json!({}),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

struct RecallNoop;
impl LlmClient for RecallNoop {
    fn complete(&self, _i: LlmInput) -> Result<LlmOutput> {
        Ok(LlmOutput {
            provider: "t".into(),
            model: "t".into(),
            content: "ok".into(),
            journal_payload: json!({}),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

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
        extra_allowed_operations: vec![],
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

// =========================================================================
// Test: ProviderToolTurn + follow_ups chain
// =========================================================================

#[test]
fn recall_provider_turn_and_follow_up_chain() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Seed a message.
    let envelope = serde_json::from_value(json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "ingress_seed",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_recall", "sender_type": "user",
            "chat_id": "chat_r", "chat_type": "p2p",
            "message_id": "msg_seed", "message_type": "text",
            "text": "SEED_HISTORY", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    }))?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    Runtime::new(config.clone(), RecallNoop).deliver(&journal, &gateway, event)?;

    // Recall run.
    let recall_env = serde_json::from_value(json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "ingress_recall",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_recall", "sender_type": "user",
            "chat_id": "chat_r", "chat_type": "p2p",
            "message_id": "msg_recall", "message_type": "text",
            "text": "run recall", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    }))?;
    let recall_event = gateway.validate_ingress(&journal, recall_env)?;

    let capturing = CapturingRecallLlm::new();
    let inputs_arc = capturing.inputs.clone();
    Runtime::new(config, capturing).deliver(&journal, &gateway, recall_event)?;

    let inputs = inputs_arc.lock().unwrap();
    assert_eq!(inputs.len(), 2, "must have 2 rounds");
    assert!(inputs[0].follow_ups.is_empty(), "round 1 no follow_ups");
    assert!(
        !inputs[1].follow_ups.is_empty(),
        "round 2 must have follow_ups"
    );
    assert!(
        inputs[1]
            .follow_ups
            .iter()
            .any(|fu| { fu.provider_turn.provider_tool_call_id == "recall_call_1" }),
        "round 2 follow_ups must contain recall_call_1"
    );

    // Receipt.
    let receipt = journal
        .events()?
        .into_iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .last()
        .expect("ReceiptReceived");
    assert_eq!(
        receipt.payload.get("status").and_then(|v| v.as_str()),
        Some("Succeeded")
    );

    // Receipt output must NOT contain session_id.
    let output = receipt.payload.get("output").cloned().unwrap_or_default();
    assert!(
        output.get("session_id").is_none(),
        "Receipt output must not contain session_id: {:?}",
        output
    );

    // If recall returned messages, verify strict key set.
    if let Some(messages) = output.get("messages").and_then(|m| m.as_array()) {
        if !messages.is_empty() {
            let allowed: BTreeSet<&str> = BTreeSet::from(["event_id", "role", "text"]);
            for msg in messages {
                let keys: BTreeSet<&str> = msg
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(|k| k.as_str())
                    .collect();
                assert_eq!(
                    keys, allowed,
                    "keys must be {{event_id,role,text}}: {keys:?}"
                );
            }
        }
    }

    // ToolResult and Provider round 2 must not contain session_id.
    for fu in &inputs[1].follow_ups {
        let result_str = &fu.result_content;
        assert!(
            !result_str.contains("session_id"),
            "ToolResult must not contain session_id: {result_str}"
        );
    }

    Ok(())
}
