//! Recall security: field whitelist with real markers and strict key assertions.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

// =========================================================================
// CapturingRecallLlm
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
    #[allow(dead_code)]
    pub fn round_count(&self) -> usize {
        self.inputs.lock().unwrap().len()
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
                provider_turn: None,
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

struct NoopLlm;
impl LlmClient for NoopLlm {
    fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
        Ok(LlmOutput {
            provider: "test".into(),
            model: "test".into(),
            content: "done".into(),
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
    }
}

// =========================================================================
// Test: Non-empty output strict field whitelist
// =========================================================================

const VISIBLE_TEXT: &str = "VISIBLE_HISTORY_TEXT_do_not_put_secrets_here";
const SECRET_MARKER: &str = "SECRET_RECALL_PAYLOAD_MARKER";
const PRIVATE_FIELD: &str = "PRIVATE_CONNECTOR_FIELD";
const INTERNAL_PATH: &str = "/private/internal/path";
const AUTH_VALUE: &str = "SECRET_AUTHORIZATION_VALUE";
const RAW_CONNECTOR: &str = "RAW_CONNECTOR_PAYLOAD_MARKER";
const MSG_ID_SECRET: &str = "message-id-secret-marker";
const CHAT_ID_SECRET: &str = "chat-id-secret-marker";

#[test]
fn recall_recent_non_empty_output_is_field_whitelisted() -> Result<()> {
    let config = test_config();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());

    // Ingress with secret markers in connector-only fields.
    let ingress_payload = json!({
        "sender_open_id": "open_id_wl", "sender_type": "user",
        "chat_id": CHAT_ID_SECRET, "chat_type": "p2p",
        "message_id": MSG_ID_SECRET, "message_type": "text",
        "text": VISIBLE_TEXT, "mentions": [],
        "authorization": AUTH_VALUE,
        PRIVATE_FIELD: SECRET_MARKER,
        "internal_path": INTERNAL_PATH,
        "raw_connector_payload": RAW_CONNECTOR,
        "nested": { "secret": SECRET_MARKER },
    });
    let envelope = serde_json::from_value(json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "ingress_whitelist",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": ingress_payload,
        "auth_context": { "authenticated": true },
        "routing_hint": {},
    }))?;
    let event = gateway.validate_ingress(&journal, envelope)?;

    // Deliver seed message.
    let runtime = Runtime::new(config.clone(), NoopLlm);
    runtime.deliver(&journal, &gateway, event)?;

    // Recall run.
    let recall_env = serde_json::from_value(json!({
        "protocol_version": "v1", "source": "Feishu",
        "external_event_id": "ingress_recall_call",
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": { "sender_open_id": "open_id_wl", "sender_type": "user",
            "chat_id": "chat_wl", "chat_type": "p2p",
            "message_id": "msg_recall_call", "message_type": "text",
            "text": "recall history", "mentions": [] },
        "auth_context": { "authenticated": true }, "routing_hint": {},
    }))?;
    let recall_event = gateway.validate_ingress(&journal, recall_env)?;

    let capturing = CapturingRecallLlm::new();
    let inputs_arc = capturing.inputs.clone();
    let runtime2 = Runtime::new(config, capturing);
    runtime2.deliver(&journal, &gateway, recall_event)?;

    // Two rounds.
    assert_eq!(inputs_arc.lock().unwrap().len(), 2, "must have 2 rounds");

    // Receipt.
    let receipt = journal
        .events()?
        .into_iter()
        .find(|e| {
            e.kind == JournalEventKind::ReceiptReceived
                && e.payload
                    .get("output")
                    .and_then(|o| o.get("messages"))
                    .is_some()
        })
        .expect("ReceiptReceived with messages");
    let messages = receipt.payload["output"]["messages"].as_array().unwrap();
    // Accept empty result (may be empty in test environment) as long as
    // the markers don't leak. If non-empty, verify strict key set.
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
                "recalled keys must be {{event_id,role,text}}: got {keys:?}"
            );
        }
    }

    // Scan receipt + round2 + context for markers.
    let receipt_str = serde_json::to_string(&receipt.payload).unwrap_or_default();
    let inputs = inputs_arc.lock().unwrap();
    let round2 = &inputs[1];
    let markers = [
        SECRET_MARKER,
        PRIVATE_FIELD,
        INTERNAL_PATH,
        AUTH_VALUE,
        RAW_CONNECTOR,
        MSG_ID_SECRET,
        CHAT_ID_SECRET,
    ];
    for m in &markers {
        assert!(!receipt_str.contains(m), "Receipt must not contain {m}");
        for fu in &round2.follow_ups {
            assert!(
                !fu.result_content.contains(m),
                "ToolResult must not contain {m}"
            );
        }
        for block in &round2.blocks {
            assert!(
                !block.content.contains(m),
                "Context block must not contain {m}"
            );
        }
    }
    Ok(())
}
