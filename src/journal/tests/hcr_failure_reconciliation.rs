use super::*;
use crate::domain::{
    AgentId, ChannelKind, EventId, InvocationId, JournalEventKind, PrincipalId, PrincipalSource,
    PrincipalSubject, Run, RunId, RunMode, RunPrincipal, RunStatus, SessionTarget,
};
use chrono::Utc;
use rusqlite::params;
use serde_json::json;

struct Fixture {
    journal: JournalStore,
    hcr_id: String,
    claim_id: String,
    run_id: String,
    origin_run_id: RunId,
    source_message_id: String,
    development_request_id: String,
    invocation_id: InvocationId,
}

fn fixture() -> Fixture {
    let journal = JournalStore::in_memory().unwrap();
    let source_message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let development_request_id = format!("devreq_{}", uuid::Uuid::new_v4().simple());
    let requirement = json!({
        "candidate_id": "candidate_test",
        "development_request": {
            "request_id": development_request_id,
            "source_message_id": source_message_id,
        }
    })
    .to_string();
    let (hcr_id, _) = journal
        .create_harness_change_request(
            "Feishu",
            &source_message_id,
            "session_origin",
            "feishu:open_id:test",
            "Feishu",
            "p2p",
            "generic-harness",
            &requirement,
        )
        .unwrap();
    let claim_id = journal
        .claim_hcr_for_execution(&hcr_id, "generic-harness", "kernel_hcr_accept")
        .unwrap()
        .0;
    let run_id = RunId::new();
    journal
        .create_hcr_run_binding(&hcr_id, &claim_id, &run_id.0)
        .unwrap();
    let session = journal
        .get_or_create_session(&SessionTarget {
            agent_id: AgentId::new(),
            channel: ChannelKind::Cli,
            conversation_key: format!("test-{hcr_id}"),
        })
        .unwrap();
    let snapshot_id = journal.current_registry_snapshot_id().unwrap();
    journal
        .create_hcr_run(&Run {
            id: run_id.clone(),
            session_id: session.id,
            agent_id: session.agent_id,
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("test-principal".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: None,
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            registry_snapshot_id: snapshot_id,
            mode: RunMode::Hcr {
                hcr_id: hcr_id.clone(),
                harness_id: "generic-harness".into(),
                claim_id: claim_id.clone(),
            },
        })
        .unwrap();

    let origin_run_id = RunId::new();
    let invocation_id = InvocationId::new();
    journal
        .claim_coding_task_submission(
            &source_message_id,
            &format!("sha256:{}", "a".repeat(64)),
            &invocation_id,
            &origin_run_id,
            &crate::domain::SessionId("session_origin".into()),
        )
        .unwrap();
    Fixture {
        journal,
        hcr_id,
        claim_id,
        run_id: run_id.0,
        origin_run_id,
        source_message_id,
        development_request_id,
        invocation_id,
    }
}

fn append_origin_run_failure(fixture: &Fixture) -> String {
    fixture
        .journal
        .append_event(
            JournalEventKind::RunFailed,
            Some(&fixture.origin_run_id),
            None,
            Some(&fixture.source_message_id),
            json!({
                "run_id": fixture.origin_run_id.0,
                "development_request_id": fixture.development_request_id,
                "error_category": "external_infrastructure_failure",
            }),
        )
        .unwrap()
        .event_id
        .0
}

fn reconcile(fixture: &Fixture, event_id: &str) -> anyhow::Result<FailureReconciliation> {
    fixture.journal.reconcile_hcr_failure(
        &fixture.hcr_id,
        &fixture.claim_id,
        &fixture.run_id,
        event_id,
    )
}

#[test]
fn trusted_run_failure_settles_claim_and_run_append_only() {
    let fixture = fixture();
    let event_id = append_origin_run_failure(&fixture);
    let before = fixture.journal.events().unwrap();

    let result = reconcile(&fixture, &event_id).unwrap();

    assert!(!result.idempotent);
    assert_eq!(
        fixture
            .journal
            .get_harness_change_request(&fixture.hcr_id)
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
    assert!(fixture
        .journal
        .get_active_claim_for_hcr(&fixture.hcr_id)
        .unwrap()
        .is_none());
    assert_eq!(
        fixture
            .journal
            .run_status(&RunId(fixture.run_id.clone()))
            .unwrap()
            .as_deref(),
        Some("Failed")
    );
    let settlement = fixture
        .journal
        .get_settlement(&fixture.hcr_id)
        .unwrap()
        .unwrap();
    assert_eq!(settlement.result, "infrastructure_failed");
    assert_eq!(
        settlement.failure_evidence_event_id.as_deref(),
        Some(event_id.as_str())
    );
    let after = fixture.journal.events().unwrap();
    assert_eq!(after.len(), before.len() + 2);
    assert_eq!(
        before.iter().map(|event| &event.hash).collect::<Vec<_>>(),
        after[..before.len()]
            .iter()
            .map(|event| &event.hash)
            .collect::<Vec<_>>()
    );
}

#[test]
fn missing_or_untrusted_failure_evidence_is_rejected() {
    let fixture = fixture();
    let event = fixture
        .journal
        .append_event(
            JournalEventKind::RunCompleted,
            Some(&fixture.origin_run_id),
            None,
            None,
            json!({"run_id": fixture.origin_run_id.0}),
        )
        .unwrap();
    let error = reconcile(&fixture, &event.event_id.0).unwrap_err();
    assert!(error.to_string().contains("EVIDENCE_NOT_TRUSTED"));
    assert!(fixture
        .journal
        .get_active_claim_for_hcr(&fixture.hcr_id)
        .unwrap()
        .is_some());
}

#[test]
fn active_kernel_lease_is_rejected() {
    let fixture = fixture();
    let event_id = append_origin_run_failure(&fixture);
    let now = Utc::now();
    fixture
        .journal
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO worker_jobs (
                job_id,job_type,source_event_id,run_id,status,attempts,available_at,
                locked_by,locked_until,created_at,updated_at
             ) VALUES ('job_active','test','source',?1,'running',1,?2,'worker',?3,?2,?2)",
            params![
                fixture.run_id,
                now.to_rfc3339(),
                (now + chrono::Duration::minutes(5)).to_rfc3339()
            ],
        )
        .unwrap();
    let error = reconcile(&fixture, &event_id).unwrap_err();
    assert!(error.to_string().contains("ACTIVE_LEASE_OR_WORK"));
}

#[test]
fn repeated_reconciliation_is_idempotent() {
    let fixture = fixture();
    let event_id = append_origin_run_failure(&fixture);
    let first = reconcile(&fixture, &event_id).unwrap();
    let event_count = fixture.journal.event_count().unwrap();
    let second = reconcile(&fixture, &event_id).unwrap();
    assert!(second.idempotent);
    assert_eq!(first.settlement_id, second.settlement_id);
    assert_eq!(fixture.journal.event_count().unwrap(), event_count);
}

#[test]
fn candidate_passed_fact_cannot_be_reconciled_as_failed() {
    let fixture = fixture();
    let event_id = append_origin_run_failure(&fixture);
    fixture
        .journal
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO hcr_receipt_identities (
                hcr_id,claim_id,run_id,idempotency_key,payload_digest,receipt_event_id,
                harness_execution_id,overall_outcome,candidate_digest,artifact_ref,
                artifact_digest,evidence_digest,candidate_id,invocation_id
             ) VALUES (?1,?2,?3,'key','digest','receipt','execution','CandidatePassed',
                ?4,?4,?4,?4,'candidate','invocation')",
            params![
                fixture.hcr_id,
                fixture.claim_id,
                fixture.run_id,
                format!("sha256:{}", "b".repeat(64))
            ],
        )
        .unwrap();
    let error = reconcile(&fixture, &event_id).unwrap_err();
    assert!(error.to_string().contains("SUCCESS_FACT_EXISTS"));
}

#[test]
fn bound_failed_gate_receipt_is_trusted() {
    let fixture = fixture();
    let intent_id = format!("intent_{}", uuid::Uuid::new_v4().simple());
    fixture
        .journal
        .insert_gate_attempt(
            "attempt_1",
            &fixture.hcr_id,
            &fixture.claim_id,
            &fixture.run_id,
            "generic-harness",
            "workspace",
            "trusted_smoke",
            "generic.smoke",
            "trusted",
            &intent_id,
            &Utc::now().to_rfc3339(),
        )
        .unwrap();
    let receipt = fixture
        .journal
        .append_event(
            JournalEventKind::ReceiptReceived,
            Some(&RunId(fixture.run_id.clone())),
            None,
            Some(&intent_id),
            json!({
                "status": "Failed",
                "output": {"error_category": "SMOKE_FAILED"}
            }),
        )
        .unwrap();
    fixture
        .journal
        .insert_evidence_atomically(
            "evidence_1",
            "attempt_1",
            &receipt.event_id.0,
            "digest",
            &Utc::now().to_rfc3339(),
        )
        .unwrap();
    assert!(reconcile(&fixture, &receipt.event_id.0).is_ok());
}

#[test]
fn failed_submit_receipt_is_trusted() {
    let fixture = fixture();
    fixture
        .journal
        .append_event(
            JournalEventKind::InvocationProposed,
            Some(&fixture.origin_run_id),
            None,
            Some(&fixture.invocation_id.0),
            json!({"operation": "external.coding_task_submit"}),
        )
        .unwrap();
    fixture
        .journal
        .append_event(
            JournalEventKind::InvocationApproved,
            Some(&fixture.origin_run_id),
            None,
            Some(&fixture.invocation_id.0),
            json!({"operation": "external.coding_task_submit"}),
        )
        .unwrap();
    let receipt = fixture
        .journal
        .append_event(
            JournalEventKind::ReceiptReceived,
            Some(&fixture.origin_run_id),
            None,
            Some(&fixture.invocation_id.0),
            json!({
                "invocation_id": fixture.invocation_id.0,
                "operation": "external.coding_task_submit",
                "status": "Failed",
                "output": {
                    "detail_code": "MISSING_DH_READ_TOKEN",
                    "error_category": "external_infrastructure_failure"
                }
            }),
        )
        .unwrap();
    assert!(reconcile(&fixture, &receipt.event_id.0).is_ok());
}
