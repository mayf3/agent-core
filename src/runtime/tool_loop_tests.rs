use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::ToolCall;
use crate::runtime::tool_rejection::sanitize_operation_for_audit;
use crate::runtime::Runtime;
use serde_json::json;
use std::path::PathBuf;

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
        coding_harness_api_url: "http://127.0.0.1:7200".into(),
        coding_harness_artifact_digest:
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        capability_submit_token: None,
        capability_decision_token: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
    }
}

/// One-call fixture: (journal, gateway, runtime, session, run) with a
/// principal granted `system.status` + `session.recall_recent`.
fn fixture() -> (
    JournalStore,
    Gateway,
    Runtime<crate::llm::LocalEchoLlm>,
    Session,
    Run,
) {
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
            grants: vec![
                CapabilityGrant {
                    operation: "system.status".into(),
                    scope: "current_session".into(),
                },
                CapabilityGrant {
                    operation: "session.recall_recent".into(),
                    scope: "current_session".into(),
                },
            ],
            requester_id: Some("cli:local".into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: now,
        updated_at: now,
        registry_snapshot_id: String::new(),
        mode: RunMode::Default,
    };
    (journal, gateway, runtime, session, run)
}

fn count(events: &[JournalEvent], kind: JournalEventKind) -> usize {
    events.iter().filter(|e| e.kind == kind).count()
}

// ===== §1/§9: rejected tool call → Issued+Rejected, no Receipt =====
#[test]
fn rejected_tool_call_writes_issued_and_rejected_not_invocation() {
    let (journal, gateway, runtime, session, run) = fixture();
    let bad_op = ToolCall {
        id: "bad_op".into(),
        operation: "shell.exec".into(),
        arguments: json!({}),
    };
    assert!(runtime
        .handle_inline_tool_call(
            &journal,
            &gateway,
            &run,
            &session,
            &bad_op,
            0,
            0,
            &crate::registry::snapshot::test_snapshot()
        )
        .is_ok());
    let events = journal.events().unwrap();
    assert_eq!(count(&events, JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count(&events, JournalEventKind::ToolCallRejected), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationProposed), 0);
    assert_eq!(count(&events, JournalEventKind::InvocationApproved), 0);
    assert_eq!(count(&events, JournalEventKind::ReceiptReceived), 0);
    let rejected = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ToolCallRejected)
        .unwrap();
    assert_eq!(
        rejected
            .payload
            .get("error_category")
            .and_then(|v| v.as_str()),
        Some("unknown_operation")
    );
    let audited = rejected
        .payload
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap();
    assert!(
        audited.starts_with("unknown_operation_"),
        "sanitized: {audited}"
    );
    assert!(!audited.contains("shell.exec"), "raw op leaked: {audited}");
}

// ===== §2: successful tool call → Proposed+Approved+Succeeded Receipt =====
#[test]
fn successful_tool_call_writes_proposed_approved_succeeded_receipt() {
    let (journal, gateway, runtime, session, run) = fixture();
    let tc = ToolCall {
        id: "tc1".into(),
        operation: "system.status".into(),
        arguments: json!({}),
    };
    assert!(runtime
        .handle_inline_tool_call(
            &journal,
            &gateway,
            &run,
            &session,
            &tc,
            0,
            0,
            &crate::registry::snapshot::test_snapshot()
        )
        .is_ok());
    let events = journal.events().unwrap();
    assert_eq!(count(&events, JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationProposed), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationApproved), 1);
    assert_eq!(count(&events, JournalEventKind::ReceiptReceived), 1);
    let receipt = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ReceiptReceived)
        .unwrap();
    assert_eq!(
        receipt.payload.get("status").and_then(|s| s.as_str()),
        Some("Succeeded")
    );
}

// ===== §2/§3: capability failure → exactly one Failed Receipt (real chain) =====
#[test]
fn capability_failure_writes_failed_receipt_not_running() {
    let (journal, gateway, runtime, session, run) = fixture();
    journal.insert_run(&run).unwrap();
    journal.set_recall_failure_for_test(true);
    let tc = ToolCall {
        id: "recall_fail".into(),
        operation: "session.recall_recent".into(),
        arguments: json!({}),
    };
    assert!(
        runtime
            .handle_inline_tool_call(
                &journal,
                &gateway,
                &run,
                &session,
                &tc,
                0,
                0,
                &crate::registry::snapshot::test_snapshot()
            )
            .is_ok(),
        "capability failure is a ToolResult, not Err"
    );
    let events = journal.events().unwrap();
    assert_eq!(count(&events, JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationProposed), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationApproved), 1);
    assert_eq!(count(&events, JournalEventKind::ReceiptReceived), 1);
    let receipt = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ReceiptReceived)
        .unwrap();
    assert_eq!(
        receipt.payload.get("status").and_then(|s| s.as_str()),
        Some("Failed")
    );
    let output = receipt.payload.get("output").unwrap();
    assert!(
        output.get("messages").is_none(),
        "failed receipt != empty success"
    );
    assert!(
        output.get("error_category").is_some(),
        "error category present"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .filter(|e| e.payload.get("status").and_then(|s| s.as_str()) == Some("Failed"))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .filter(|e| e.payload.get("status").and_then(|s| s.as_str()) == Some("Succeeded"))
            .count(),
        0
    );
    let j = serde_json::to_string(&events).unwrap();
    assert!(
        !j.contains("sqlite")
            && !j.contains("journal_events")
            && !j.contains("recall_query_failed")
    );
}

/// Empty recall → Succeeded + `messages: []` (differs from a DB error).
#[test]
fn empty_recall_returns_succeeded_empty_messages() {
    let (journal, gateway, runtime, session, run) = fixture();
    let tc = ToolCall {
        id: "recall_empty".into(),
        operation: "session.recall_recent".into(),
        arguments: json!({}),
    };
    assert!(runtime
        .handle_inline_tool_call(
            &journal,
            &gateway,
            &run,
            &session,
            &tc,
            0,
            0,
            &crate::registry::snapshot::test_snapshot()
        )
        .is_ok());
    let events = journal.events().unwrap();
    let receipt = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ReceiptReceived)
        .unwrap();
    assert_eq!(
        receipt.payload.get("status").and_then(|s| s.as_str()),
        Some("Succeeded")
    );
    let messages = receipt
        .payload
        .get("output")
        .unwrap()
        .get("messages")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(messages.is_empty());
}

// ===== §1.3: provider malformed → Issued+Rejected, safe internal id =====
#[test]
fn malformed_tool_call_writes_issued_rejected_with_safe_internal_id() {
    let (journal, _gateway, runtime, session, run) = fixture();
    let outcome = runtime
        .handle_malformed_tool_call(&journal, &run, &session, 0, 0)
        .unwrap();
    assert!(matches!(
        outcome,
        crate::runtime::tool_loop::ToolCallOutcome::ToolResult { .. }
    ));
    let events = journal.events().unwrap();
    assert_eq!(count(&events, JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count(&events, JournalEventKind::ToolCallRejected), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationProposed), 0);
    assert_eq!(count(&events, JournalEventKind::InvocationApproved), 0);
    assert_eq!(count(&events, JournalEventKind::ReceiptReceived), 0);
    let issued = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ToolCallIssued)
        .unwrap();
    let tcid = issued
        .payload
        .get("tool_call_id")
        .and_then(|v| v.as_str())
        .unwrap();
    assert!(tcid.starts_with("tc:"), "position-derived id: {tcid}");
    assert_eq!(
        issued.payload.get("operation").and_then(|v| v.as_str()),
        Some("malformed_tool_call")
    );
    let j = serde_json::to_string(&events).unwrap();
    assert!(!j.contains("missing function") && !j.contains("arguments JSON parse error"));
}

// ===== §5: untrusted operation never leaks raw into Journal =====
#[test]
fn untrusted_operation_never_leaks_raw_into_journal() {
    let cases = [
        ("overlong", "x".repeat(10_000)),
        ("unicode", "操作🔥工具".to_string()),
        ("control", "op\nwith\r\tcontrol".to_string()),
        ("path", "../../../etc/passwd".to_string()),
        (
            "token",
            "credential_marker_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890".to_string(),
        ),
        ("auth", "header_marker_supersecret".to_string()),
    ];
    for (label, raw_op) in cases {
        let (journal, gateway, runtime, session, run) = fixture();
        let tc = ToolCall {
            id: "leak".into(),
            operation: raw_op.clone(),
            arguments: json!({}),
        };
        let _ = runtime.handle_inline_tool_call(
            &journal,
            &gateway,
            &run,
            &session,
            &tc,
            0,
            0,
            &crate::registry::snapshot::test_snapshot(),
        );
        let j = serde_json::to_string(&journal.events().unwrap()).unwrap();
        assert!(!j.contains(&raw_op), "[{}] raw leaked", label);
        assert!(
            !j.contains("credential_marker")
                && !j.contains("header_marker")
                && !j.contains("passwd"),
            "[{}] sensitive leaked",
            label
        );
    }
}

#[test]
fn sanitize_operation_keeps_catalog_and_collapses_unknown() {
    assert_eq!(
        sanitize_operation_for_audit("system.status"),
        "system.status"
    );
    let s = sanitize_operation_for_audit("shell.exec");
    assert!(s.starts_with("unknown_operation_"));
    assert_eq!(
        sanitize_operation_for_audit("shell.exec"),
        sanitize_operation_for_audit("shell.exec")
    );
}

// ===== §6: idempotency key composition (turn + tool_index) =====
#[test]
fn idempotency_key_is_run_turn_index_scoped() {
    use crate::gateway::validate_tool_call;
    use crate::llm::tool_call_id_hash;
    use crate::registry::snapshot::test_snapshot;
    let raw_id = "call_abc123";
    let hashed = tool_call_id_hash(raw_id);
    let mk = |op: &str| ToolCall {
        id: hashed.clone(),
        operation: op.to_string(),
        arguments: json!({}),
    };
    let run = RunId::new();
    let snap = test_snapshot();
    let k1 = validate_tool_call(&mk("system.status"), &run, 0, 0, &snap).unwrap();
    let k2 = validate_tool_call(&mk("system.status"), &run, 0, 0, &snap).unwrap();
    assert_eq!(k1.idempotency_key, k2.idempotency_key, "stable");
    assert_ne!(
        validate_tool_call(&mk("system.status"), &run, 1, 0, &snap)
            .unwrap()
            .idempotency_key,
        validate_tool_call(&mk("system.status"), &run, 0, 0, &snap)
            .unwrap()
            .idempotency_key,
        "turn"
    );
    assert_ne!(
        validate_tool_call(&mk("system.status"), &run, 0, 0, &snap)
            .unwrap()
            .idempotency_key,
        validate_tool_call(&mk("system.status"), &run, 0, 1, &snap)
            .unwrap()
            .idempotency_key,
        "index"
    );
    assert_ne!(
        validate_tool_call(&mk("system.status"), &run, 0, 0, &snap)
            .unwrap()
            .idempotency_key,
        validate_tool_call(&mk("system.status"), &RunId::new(), 0, 0, &snap)
            .unwrap()
            .idempotency_key,
        "run"
    );
    assert!(
        !k1.idempotency_key.clone().unwrap().contains(raw_id),
        "raw id leaked"
    );
}

// ===== §9: typed rejection categories =====

#[test]
fn policy_denial_writes_rejected_with_correlation() {
    let (journal, gateway, runtime, session, mut run) = fixture();
    run.principal.grants.clear();
    let tc = ToolCall {
        id: "no_grant".into(),
        operation: "system.status".into(),
        arguments: json!({}),
    };
    let _ = runtime.handle_inline_tool_call(
        &journal,
        &gateway,
        &run,
        &session,
        &tc,
        0,
        0,
        &crate::registry::snapshot::test_snapshot(),
    );
    let events = journal.events().unwrap();
    assert_eq!(count(&events, JournalEventKind::InvocationProposed), 1);
    assert_eq!(count(&events, JournalEventKind::ToolCallRejected), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationApproved), 0);
    assert_eq!(count(&events, JournalEventKind::ReceiptReceived), 0);
    let rejected = events
        .iter()
        .find(|e| e.kind == JournalEventKind::ToolCallRejected)
        .unwrap();
    assert_eq!(
        rejected
            .payload
            .get("error_category")
            .and_then(|v| v.as_str()),
        Some("policy_denied")
    );
    assert!(rejected.correlation_id.is_some());
}
