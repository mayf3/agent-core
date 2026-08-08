use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ToolCall, ToolCallResult};
use crate::runtime::tool_rejection::sanitize_operation_for_audit;
use crate::runtime::Runtime;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
        budget_hook_id: None,
        budget_hook_version: None,
        budget_decision_digest: None,
        budget_max_tool_rounds: None,
        budget_max_wall_time_ms: None,
        budget_exhaustion_action: None,
    };
    (journal, gateway, runtime, session, run)
}

fn count(events: &[JournalEvent], kind: JournalEventKind) -> usize {
    events.iter().filter(|e| e.kind == kind).count()
}

struct SameSubmissionLlm {
    calls: AtomicUsize,
    arguments: serde_json::Value,
}

impl LlmClient for SameSubmissionLlm {
    fn complete(&self, _input: LlmInput) -> anyhow::Result<LlmOutput> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if call == 0 {
            LlmOutput {
                provider: "test".into(),
                model: "same-submission".into(),
                content: String::new(),
                journal_payload: json!({"round": call}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "same_submit_second".into(),
                    operation: crate::domain::operation::external::TASK_SUBMIT.into(),
                    arguments: self.arguments.clone(),
                }),
                provider_turn: None,
            }
        } else {
            LlmOutput {
                provider: "test".into(),
                model: "same-submission".into(),
                content: "done".into(),
                journal_payload: json!({"round": call}),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            }
        })
    }
}

struct EnvRestore {
    key: &'static str,
    previous: Option<String>,
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        if let Some(value) = self.previous.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn start_rejecting_coding_harness(
    calls: Arc<AtomicUsize>,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind Coding Harness test port");
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while calls.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("accept Coding Harness request: {error}"),
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = stream.read(&mut chunk).expect("read Harness request");
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..read]);
                let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or_default();
                if bytes.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            calls.fetch_add(1, Ordering::SeqCst);
            let body = json!({
                "protocol_version": "external-harness-v1",
                "ok": false,
                "outcome": "definitively_rejected",
                "error_code": "GENERIC_DEFINITIVE_REJECTION"
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write Harness response");
        }
    });
    (address, handle)
}

#[test]
fn definitive_rejection_allows_byte_identical_submission_to_reach_harness_again() {
    let harness_calls = Arc::new(AtomicUsize::new(0));
    let (harness_addr, harness) = start_rejecting_coding_harness(harness_calls.clone());
    let previous_token = std::env::var("AGENT_CORE_CODING_HARNESS_CONTROL_TOKEN").ok();
    std::env::set_var(
        "AGENT_CORE_CODING_HARNESS_CONTROL_TOKEN",
        "test-control-token",
    );
    let _restore = EnvRestore {
        key: "AGENT_CORE_CODING_HARNESS_CONTROL_TOKEN",
        previous: previous_token,
    };
    let previous_addr = std::env::var("AGENT_CORE_TEST_CODING_HARNESS_ADDR").ok();
    std::env::set_var("AGENT_CORE_TEST_CODING_HARNESS_ADDR", harness_addr);
    let _restore_addr = EnvRestore {
        key: "AGENT_CORE_TEST_CODING_HARNESS_ADDR",
        previous: previous_addr,
    };

    let owner = "owner_same_submission";
    let mut config = test_config();
    config.feishu_coding_owner_id = Some(owner.into());
    config.harness_artifact_root = std::env::temp_dir().join(format!(
        "same_submission_runtime_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let journal = JournalStore::in_memory().unwrap();
    let session = journal
        .get_or_create_session(&SessionTarget {
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: format!("feishu:open_id:{owner}"),
        })
        .unwrap();
    let source_event_id = EventId::new();
    journal
        .append_event(
            JournalEventKind::IngressAccepted,
            None,
            Some(&session.id),
            Some("message_same_submission"),
            json!({
                "event_id": source_event_id.0,
                "source": "feishu",
                "message_id": "message_same_submission",
                "text": "submit"
            }),
        )
        .unwrap();
    let snapshot_id = journal.current_registry_snapshot_id().unwrap();
    let snapshot = journal.load_registry_snapshot(&snapshot_id).unwrap();
    let now = chrono::Utc::now();
    let run = Run {
        id: RunId::new(),
        session_id: session.id.clone(),
        agent_id: AgentId("main".into()),
        trigger_event_id: source_event_id,
        principal: RunPrincipal {
            principal_id: PrincipalId(format!("feishu:open_id:{owner}")),
            subject: PrincipalSubject::FeishuOpenId(owner.into()),
            source: PrincipalSource::Feishu,
            grants: vec![CapabilityGrant {
                operation: crate::domain::operation::external::TASK_SUBMIT.into(),
                scope: "current_session".into(),
            }],
            requester_id: Some(owner.into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: now,
        updated_at: now,
        registry_snapshot_id: snapshot_id,
        mode: RunMode::Default,
        budget_hook_id: None,
        budget_hook_version: None,
        budget_decision_digest: None,
        budget_max_tool_rounds: Some(3),
        budget_max_wall_time_ms: Some(30_000),
        budget_exhaustion_action: Some(crate::hook::ExhaustionAction::Terminate),
    };
    journal.insert_run(&run).unwrap();
    let arguments = json!({
        "development_request": {
            "target_kind": "invocable_capability",
            "name": "external.same_submission",
            "requirements": ["provide a bounded capability"],
            "required_contracts": ["component.invoke.v0"],
            "acceptance_criteria": ["profile gates pass"]
        }
    });
    let runtime = Runtime::new(
        config.clone(),
        SameSubmissionLlm {
            calls: AtomicUsize::new(0),
            arguments: arguments.clone(),
        },
    );
    let gateway = Gateway::new(config);
    let initial = LlmOutput {
        provider: "test".into(),
        model: "same-submission".into(),
        content: String::new(),
        journal_payload: json!({"round": "initial"}),
        tool_call: ToolCallResult::Valid(ToolCall {
            id: "same_submit_first".into(),
            operation: crate::domain::operation::external::TASK_SUBMIT.into(),
            arguments,
        }),
        provider_turn: None,
    };
    let final_output = runtime
        .run_tool_recall_loop(
            &journal,
            &gateway,
            &run,
            &session,
            &mut Vec::new(),
            "submit",
            initial,
            &snapshot,
        )
        .unwrap();
    assert!(final_output.tool_call.is_absent());
    harness.join().unwrap();
    let events = journal.events().unwrap();
    assert_eq!(
        harness_calls.load(Ordering::SeqCst),
        2,
        "events={} final_content={}",
        serde_json::to_string(&events).unwrap(),
        final_output.content
    );
    assert_eq!(count(&events, JournalEventKind::ToolLoopDetected), 0);
    let conn = journal.conn.lock().unwrap();
    let attempts: Vec<(i64, String)> = conn
        .prepare(
            "SELECT attempt_sequence,status FROM coding_task_submissions
             WHERE source_message_id='message_same_submission'
             ORDER BY attempt_sequence",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        attempts,
        vec![
            (1, "definitively_rejected".into()),
            (2, "definitively_rejected".into())
        ]
    );
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
            &crate::registry::snapshot::test_snapshot(),
            10_000,
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
            &crate::registry::snapshot::test_snapshot(),
            10_000,
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
                &crate::registry::snapshot::test_snapshot(),
                10_000,
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
            &crate::registry::snapshot::test_snapshot(),
            10_000,
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
            10_000,
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
        10_000,
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

// ===== Third cut: narrow invocation entry (run_id only) =====
/// Narrow-entry fixture: journal with the baseline snapshot and a
/// PERSISTED session/run, so the narrow entry can reload the legacy
/// governance objects from the journal by run_id.
fn narrow_fixture() -> (
    JournalStore,
    Gateway,
    Runtime<crate::llm::LocalEchoLlm>,
    Run,
) {
    let config = test_config();
    let journal = JournalStore::in_memory().unwrap();
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, crate::llm::LocalEchoLlm);
    let session = journal
        .get_or_create_session(&SessionTarget {
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "local".into(),
        })
        .unwrap();
    let snapshot_id = journal.current_registry_snapshot_id().unwrap();
    let now = chrono::Utc::now();
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
        registry_snapshot_id: snapshot_id,
        mode: RunMode::Default,
        budget_hook_id: None,
        budget_hook_version: None,
        budget_decision_digest: None,
        budget_max_tool_rounds: None,
        budget_max_wall_time_ms: None,
        budget_exhaustion_action: None,
    };
    journal.insert_run(&run).unwrap();
    (journal, gateway, runtime, run)
}

/// Only `run_id` crosses the seam; the narrow entry reloads the legacy
/// governance objects internally and produces the same policy + invocation
/// + receipt behaviour as the original object-passing path.
#[test]
fn invoke_tool_by_run_id_only_produces_equivalent_invocation_path() {
    let (journal, gateway, runtime, run) = narrow_fixture();
    let tc = ToolCall {
        id: "tc_narrow".into(),
        operation: "system.status".into(),
        arguments: json!({}),
    };
    let outcome = runtime
        .invoke_tool(&journal, &gateway, &run.id, &tc, 0, 0, 10_000)
        .expect("narrow invocation entry");
    assert!(
        matches!(
            outcome,
            crate::runtime::tool_loop::ToolCallOutcome::ToolResult { .. }
        ),
        "expected tool result: {outcome:?}"
    );
    let events = journal.events().unwrap();
    assert_eq!(count(&events, JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationProposed), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationApproved), 1);
    assert_eq!(count(&events, JournalEventKind::ReceiptReceived), 1);
    assert_eq!(count(&events, JournalEventKind::ToolCallRejected), 0);
    let proposed = events
        .iter()
        .find(|e| e.kind == JournalEventKind::InvocationProposed)
        .unwrap();
    assert_eq!(
        proposed
            .payload
            .get("operation")
            .and_then(|v| v.as_str()),
        Some("system.status")
    );
}

/// The malformed tool-call path uses the same run_id-only boundary.
#[test]
fn invoke_malformed_tool_by_run_id_only_writes_issued_and_rejected() {
    let (journal, gateway, runtime, run) = narrow_fixture();
    let outcome = runtime
        .invoke_malformed_tool(&journal, &run.id, 0, 0)
        .expect("narrow malformed entry");
    assert!(
        matches!(
            outcome,
            crate::runtime::tool_loop::ToolCallOutcome::ToolResult { .. }
        ),
        "expected rejection result: {outcome:?}"
    );
    let events = journal.events().unwrap();
    assert_eq!(count(&events, JournalEventKind::ToolCallIssued), 1);
    assert_eq!(count(&events, JournalEventKind::ToolCallRejected), 1);
    assert_eq!(count(&events, JournalEventKind::InvocationProposed), 0);
    assert_eq!(count(&events, JournalEventKind::ReceiptReceived), 0);
    let _ = gateway;
}
