//! Phase 2 M2e: the first read-only local adapter (`time.now`) walks the full
//! intent → policy(approve) → adapter → receipt chain end-to-end.
//!
//! `time.now` is catalogued `Risk::ReadOnly` (no side effect), so the policy
//! pipeline must approve it when the principal holds the grant and targets its
//! own session, and the `TimeAdapter` must return a `Succeeded` receipt with a
//! valid timestamp. This validates the Phase 2 contract for a read-only
//! operation without touching durable state.
//!
//! See `docs/decisions/phase2-invocation-gateway-scoping.md` (M2e).

mod common;

use agent_core_kernel::adapters::{InvocationAdapter, TimeAdapter};
use agent_core_kernel::domain::operation::TIME_NOW;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use anyhow::Result;
use serde_json::json;

/// A principal granted `time.now`, targeting its own session.
fn run_with_time_grant(session_id: &SessionId) -> Run {
    Run {
        id: RunId::new(),
        session_id: session_id.clone(),
        agent_id: AgentId("main".to_string()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:local".to_string()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![CapabilityGrant {
                operation: TIME_NOW.to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("cli:local".to_string()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn time_now_is_catalogued_as_read_only() {
    // The exit criterion's foundation: time.now is in the catalog as ReadOnly.
    let spec = agent_core_kernel::domain::operation::lookup(TIME_NOW)
        .expect("time.now is catalogued");
    assert_eq!(spec.risk, agent_core_kernel::domain::operation::Risk::ReadOnly);
}

#[test]
fn time_now_walks_intent_policy_adapter_receipt() -> Result<()> {
    // End-to-end: build an intent for time.now against the run's own session,
    // approve it through the gateway policy pipeline, execute it through the
    // TimeAdapter, and assert a Succeeded receipt with a valid timestamp.
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let session = common::test_session(&config);
    let run = run_with_time_grant(&session.id);

    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: TIME_NOW.to_string(),
        arguments: json!({ "session_id": session.id.0 }),
        idempotency_key: Some("time:1".to_string()),
    };

    // Policy pipeline must Allow: grant present, operation catalogued, session
    // matches.
    let approved = gateway.approve_invocation(intent, &run, &session)?;
    assert_eq!(approved.intent().operation, TIME_NOW);

    // Adapter executes the approved invocation.
    let receipt = TimeAdapter.execute(&approved)?;
    assert_eq!(receipt.status, ReceiptStatus::Succeeded);
    assert!(receipt.external_ref.is_none());
    let iso = receipt
        .output
        .get("iso")
        .and_then(serde_json::Value::as_str)
        .expect("iso present");
    let epoch_ms = receipt
        .output
        .get("epoch_ms")
        .and_then(serde_json::Value::as_i64)
        .expect("epoch_ms present");
    assert!(epoch_ms > 1_577_836_800_000, "epoch_ms is post-2020");
    chrono::DateTime::parse_from_rfc3339(iso).expect("iso is valid RFC3339");
    Ok(())
}

#[test]
fn time_now_denied_without_grant() -> Result<()> {
    // The read-only operation still passes access control: a principal without
    // the time.now grant is denied at the policy pipeline's first stage.
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let session = common::test_session(&config);
    let mut run = run_with_time_grant(&session.id);
    run.principal.grants.clear();

    let intent = InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: run.id.clone(),
        operation: TIME_NOW.to_string(),
        arguments: json!({ "session_id": session.id.0 }),
        idempotency_key: Some("time:2".to_string()),
    };
    let err = gateway
        .approve_invocation(intent, &run, &session)
        .expect_err("denied without grant");
    assert!(
        err.to_string().contains("capability_not_enabled"),
        "expected capability_not_enabled, got: {err}"
    );
    Ok(())
}
