//! Outbox dispatch tests for failed-run notification delivery.
use crate::domain::*;
use crate::journal::JournalStore;
use serde_json::json;

#[test]
fn successful_failure_reply_dispatch_preserves_failed_run() {
    let journal = JournalStore::in_memory().unwrap();
    let run_id = RunId::new();
    let session_id = SessionId("s_fail".into());
    let inv_id = InvocationId("failure-reply:test".into());

    let run = Run {
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
        status: RunStatus::Failed,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: String::new(),
    };
    journal.insert_run(&run).unwrap();
    journal.fail_run(&run_id).unwrap();

    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: inv_id.clone(),
            run_id: run_id.clone(),
            operation: "stdout.send_text".into(),
            arguments: json!({"session_id": session_id.0, "text": "failure reply"}),
            idempotency_key: Some("failure-reply:test".into()),
        },
        "decision_fail_reply".into(),
    );
    journal
        .queue_outbox_dispatch(&approved, Some(&session_id))
        .unwrap();
    journal
        .start_outbox_dispatch(&approved, Some(&session_id))
        .unwrap();
    let receipt = Receipt {
        invocation_id: inv_id.clone(),
        status: ReceiptStatus::Succeeded,
        output: json!({"text": "delivered"}),
        external_ref: None,
        occurred_at: chrono::Utc::now(),
    };
    journal
        .succeed_outbox_dispatch(&receipt, &run_id, Some(&session_id))
        .unwrap();
    assert_eq!(
        journal.run_status(&run_id).unwrap().as_deref(),
        Some("Failed")
    );
    assert_eq!(
        journal.outbox_dispatch_status(&inv_id).unwrap().as_ref(),
        Some(&OutboxDispatchStatus::Succeeded)
    );
    let events = journal.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == JournalEventKind::RunCompleted)
            .count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .count(),
        1
    );
    assert!(journal.verify_hash_chain().unwrap());
}

#[test]
fn normal_success_dispatch_still_completes_run() {
    let journal = JournalStore::in_memory().unwrap();
    let run_id = RunId::new();
    let session_id = SessionId("s_norm".into());
    let inv_id = InvocationId("reply:normal".into());

    let run = Run {
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
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        registry_snapshot_id: String::new(),
    };
    journal.insert_run(&run).unwrap();
    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: inv_id.clone(),
            run_id: run_id.clone(),
            operation: "stdout.send_text".into(),
            arguments: json!({"session_id": session_id.0, "text": "normal reply"}),
            idempotency_key: Some("reply:normal".into()),
        },
        "decision_normal".into(),
    );
    journal
        .queue_outbox_dispatch(&approved, Some(&session_id))
        .unwrap();
    journal
        .start_outbox_dispatch(&approved, Some(&session_id))
        .unwrap();
    let receipt = Receipt {
        invocation_id: inv_id,
        status: ReceiptStatus::Succeeded,
        output: json!({"text": "delivered"}),
        external_ref: None,
        occurred_at: chrono::Utc::now(),
    };
    journal
        .succeed_outbox_dispatch(&receipt, &run_id, Some(&session_id))
        .unwrap();
    assert_eq!(
        journal.run_status(&run_id).unwrap().as_deref(),
        Some("Completed")
    );
    let events = journal.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == JournalEventKind::RunCompleted)
            .count(),
        1
    );
    assert!(journal.verify_hash_chain().unwrap());
}
