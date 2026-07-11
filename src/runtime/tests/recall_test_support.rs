//! Shared helpers for the Recall isolation / audit / no-grant Runtime tests.
//!
//! These helpers are intentionally `pub(super)` so the four REQUIRED tests
//! (which live in `recall_isolation.rs` and `recall_audit.rs`) share a single
//! faithful two-round `Runtime::deliver` provider stub and marker machinery.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::journal::JournalStore;
use crate::llm::{
    EndpointChoice, LlmClient, LlmInput, LlmOutput, ProviderToolTurn, ToolCall, ToolCallResult,
};
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// =========================================================================
// Config + snapshot fixtures
// =========================================================================

pub(super) fn test_config() -> KernelConfig {
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
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
    }
}

/// Build a registry snapshot that contains `session.recall_recent` plus the
/// reply operations so a full Runtime::deliver run can complete.
pub(super) fn recall_specs() -> Vec<OperationSpec> {
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
            name: "feishu.send_message".into(),
            risk: Risk::Write,
            description: "send feishu reply".into(),
            parameters: json!({"type": "object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.feishu_send_message".into(),
        },
        OperationSpec {
            name: "session.recall_recent".into(),
            risk: Risk::ReadOnly,
            description: "recall recent messages".into(),
            parameters: json!({"type": "object"}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.session_recall_recent".into(),
        },
        OperationSpec {
            name: "system.status".into(),
            risk: Risk::ReadOnly,
            description: "system status".into(),
            parameters: json!({"type": "object"}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.system_status".into(),
        },
    ]
}

/// Activate the recall-capable snapshot and return its id.
pub(super) fn activate_recall_snapshot(journal: &JournalStore) -> Result<String> {
    let snap = journal.create_registry_snapshot(recall_specs())?;
    let id = snap.snapshot_id.clone();
    journal.activate_registry_snapshot(&id)?;
    Ok(id)
}

/// Feishu p2p ingress envelope whose payload carries the user-visible text.
pub(super) fn feishu_envelope(
    external_event_id: &str,
    message_id: &str,
    sender_open_id: &str,
    chat_id: &str,
    text: &str,
) -> Value {
    json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": external_event_id,
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": {
            "sender_open_id": sender_open_id,
            "sender_type": "user",
            "chat_id": chat_id,
            "chat_type": "p2p",
            "message_id": message_id,
            "message_type": "text",
            "text": text,
            "mentions": []
        },
        "auth_context": { "authenticated": true },
        "routing_hint": {},
    })
}

// =========================================================================
// CapturingLlm — real ProviderToolTurn round 1, final reply round 2.
//
// Saves BOTH rounds' complete LlmInput so tests can inspect the follow-up
// chain and assert `inputs.len() == 2`.
// =========================================================================

pub(super) struct CapturingLlm {
    pub inputs: Arc<Mutex<Vec<LlmInput>>>,
    round: Mutex<usize>,
    /// Provider tool-call id used in round 1 (and matched in round 2).
    provider_call_id: &'static str,
    /// Final assistant reply text emitted in round 2.
    final_reply: &'static str,
}

impl CapturingLlm {
    pub fn new(provider_call_id: &'static str, final_reply: &'static str) -> Self {
        Self {
            inputs: Arc::new(Mutex::new(Vec::new())),
            round: Mutex::new(0),
            provider_call_id,
            final_reply,
        }
    }

    pub fn inputs_handle(&self) -> Arc<Mutex<Vec<LlmInput>>> {
        self.inputs.clone()
    }
}

impl LlmClient for CapturingLlm {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        // Round 2 builds its final reply from the tool result it just received
        // — exactly what a real model does after consuming a recall result. If
        // a follow-up is present, the reply echoes its content; otherwise it
        // falls back to the static reply. This keeps the assistant reply
        // faithful to the derived recall data (no fabricated text).
        let has_follow_up = !input.follow_ups.is_empty();
        let derived_reply = if has_follow_up {
            let mut s = String::new();
            for fu in &input.follow_ups {
                s.push_str(&fu.result_content);
                s.push('\n');
            }
            s.push_str(self.final_reply);
            s
        } else {
            self.final_reply.to_string()
        };
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
                    id: self.provider_call_id.to_string(),
                    operation: "session.recall_recent".into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(ProviderToolTurn {
                    endpoint: EndpointChoice::Primary,
                    provider_tool_call_id: self.provider_call_id.into(),
                    wire_name: "session.recall_recent".into(),
                    canonical_operation: "session.recall_recent".into(),
                    reasoning_content: None,
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(LlmOutput {
                provider: "test".into(),
                model: "test".into(),
                content: derived_reply,
                journal_payload: json!({}),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

/// Noop Llm that never emits a tool call (used to seed history).
pub(super) struct NoopLlm;
impl LlmClient for NoopLlm {
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

// =========================================================================
// Sensitive markers — must be present in the source-of-truth ingress event
// but MUST NOT appear anywhere in the recall-derived chain.
// =========================================================================

pub(super) const SENSITIVE_MARKERS: &[&str] = &[
    "SECRET_RECALL_PAYLOAD_MARKER",
    "PRIVATE_CONNECTOR_FIELD",
    "/private/internal/path",
    "SECRET_AUTHORIZATION_VALUE",
    "RAW_CONNECTOR_PAYLOAD_MARKER",
    "message-id-secret-marker",
    "chat-id-secret-marker",
    "payload_json",
    "raw_connector_payload",
    "authorization",
    "message_id",
    "chat_id",
    "session_id",
];

/// Render an `LlmInput`'s user-facing string surfaces (blocks content,
/// follow-up result content, provider-turn wire fields, user_text) into one
/// flat string for marker scanning. `LlmInput` is not Serialize by design, so
/// we concatenate its String fields directly.
pub(super) fn llm_input_blob(input: &LlmInput) -> String {
    let mut s = String::new();
    s.push_str(&input.user_text);
    for b in &input.blocks {
        s.push('\n');
        s.push_str(&b.content);
    }
    for fu in &input.follow_ups {
        s.push('\n');
        s.push_str(&fu.result_content);
        s.push('\n');
        s.push_str(&fu.provider_turn.provider_tool_call_id);
        s.push('\n');
        s.push_str(&fu.provider_turn.wire_name);
        s.push('\n');
        s.push_str(&fu.provider_turn.canonical_operation);
        s.push('\n');
        s.push_str(&fu.provider_turn.arguments_json);
    }
    s
}

/// Strict per-item key whitelist for recalled messages: exactly
/// {event_id, role, text}.
pub(super) fn assert_strict_keys(item: &Value) {
    let keys: BTreeSet<&str> = item
        .as_object()
        .unwrap_or_else(|| panic!("recall item must be an object: {item}"))
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["event_id", "role", "text"]),
        "recalled item must have exactly {{event_id, role, text}}; got {keys:?}"
    );
}

/// Shared outbox dispatch recovery test helper.
pub(super) fn run_outbox_test(status: RunStatus, expected: &str, suffix: &str) {
    let j = JournalStore::in_memory().unwrap();
    let run_id = RunId::new();
    let session_id = SessionId(format!("s_{suffix}"));
    let inv_id = InvocationId(format!("reply:{suffix}"));
    j.insert_run(&Run {
        id: run_id.clone(),
        session_id: session_id.clone(),
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
        status,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: String::new(),
        mode: RunMode::Default,
    })
    .unwrap();
    if expected == "Failed" {
        j.fail_run(&run_id).unwrap();
    }
    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: inv_id.clone(),
            run_id: run_id.clone(),
            operation: "stdout.send_text".into(),
            arguments: json!({"session_id": session_id.0, "text": suffix}),
            idempotency_key: Some(format!("reply:{suffix}")),
        },
        format!("decision_{suffix}"),
    );
    j.queue_outbox_dispatch(&approved, Some(&session_id))
        .unwrap();
    j.start_outbox_dispatch(&approved, Some(&session_id))
        .unwrap();
    j.succeed_outbox_dispatch(
        &Receipt {
            invocation_id: inv_id,
            status: ReceiptStatus::Succeeded,
            output: json!({"text": "delivered"}),
            external_ref: None,
            occurred_at: chrono::Utc::now(),
        },
        &run_id,
        Some(&session_id),
    )
    .unwrap();
    assert_eq!(j.run_status(&run_id).unwrap().as_deref(), Some(expected));
    assert_eq!(
        count_kind(&j.events().unwrap(), JournalEventKind::RunCompleted),
        if expected == "Completed" { 1 } else { 0 }
    );
    assert!(j.verify_hash_chain().unwrap());
}

/// Process the next outbox dispatch, succeeding it unconditionally.
pub(super) fn process_outbox(j: &JournalStore, run_id: &RunId) {
    if let Ok(Some(leased)) = j.lease_next_outbox_dispatch() {
        j.succeed_outbox_dispatch(
            &Receipt {
                invocation_id: leased.invocation_id,
                status: ReceiptStatus::Succeeded,
                output: json!({"text": "delivered"}),
                external_ref: None,
                occurred_at: chrono::Utc::now(),
            },
            run_id,
            leased.session_id.as_ref(),
        )
        .unwrap();
    }
}

pub(super) fn count_kind(events: &[JournalEvent], kind: JournalEventKind) -> usize {
    events.iter().filter(|e| e.kind == kind).count()
}

#[test]
fn successful_failure_reply_dispatch_preserves_failed_run() {
    run_outbox_test(RunStatus::Failed, "Failed", "fail")
}
#[test]
fn normal_success_dispatch_still_completes_run() {
    run_outbox_test(RunStatus::Running, "Completed", "norm")
}
