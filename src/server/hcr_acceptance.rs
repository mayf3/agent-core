//! Internal HCR acceptance trigger.
//!
//! `POST /internal/hcr/:hcr_id/accept`
//!
//! Orchestrates the full HCR acceptance flow:
//! 1. Load HCR
//! 2. Claim + Run binding (via existing worker)
//! 3. Gateway authorization
//! 4. HTTP call to Coding Harness acceptance endpoint
//! 5. Write ReceiptReceived event
//! 6. Call R3A settlement

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::hcr::{settlement::settle_hcr, worker::execute_hcr};
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

/// Handle a POST /internal/hcr/:hcr_id/accept request.
///
/// The request body must contain the `candidate_ref` (path to candidate
/// source directory relative to the harness artifact root).
pub fn handle(
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    hcr_id: &str,
    body: &Value,
) -> Result<Value> {
    let candidate_ref = match body.get("candidate_ref").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => c,
        _ => bail!("MISSING_CANDIDATE_REF"),
    };

    // 1. Execute HCR worker: claim + create Run binding
    let run_id = RunId::new();
    let worker_instance_id = format!("kernel_hcr_accept");
    let outcome = execute_hcr(journal, hcr_id, &run_id, &worker_instance_id)?;

    // 2. Create a minimal session for Gateway authorization
    let session_target = SessionTarget {
        agent_id: AgentId::new(),
        channel: ChannelKind::Cli,
        conversation_key: format!("hcr-accept-{hcr_id}"),
    };
    let session = journal.get_or_create_session(&session_target)?;

    // 3. Create the Run with RunMode::Hcr
    let trigger_event_id = EventId::new();
    let harness_id = {
        let hcr = journal
            .get_harness_change_request(hcr_id)?
            .ok_or_else(|| anyhow::anyhow!("HCR_NOT_FOUND"))?;
        hcr.harness_id
    };

    let principal = RunPrincipal {
        principal_id: PrincipalId::new(),
        subject: PrincipalSubject::LocalUser,
        source: PrincipalSource::Cli,
        grants: vec![CapabilityGrant {
            operation: "external.coding_hcr_accept".into(),
            scope: "hcr".into(),
        }],
        requester_id: None,
    };

    let snapshot_id = journal.current_registry_snapshot_id()?;
    let snapshot = journal.load_registry_snapshot(&snapshot_id)?;

    let run = Run {
        id: outcome.run_id.clone(),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: trigger_event_id.clone(),
        principal: principal.clone(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: snapshot_id.clone(),
        mode: RunMode::Hcr {
            hcr_id: hcr_id.to_string(),
            harness_id: harness_id.clone(),
            claim_id: outcome.claim_id.0.clone(),
        },
    };

    // Persist the Run
    journal.create_hcr_run(&run)?;

    // 4. Create InvocationIntent and get Gateway approval
    let invocation_id = InvocationId::new();
    let idempotency_key = format!(
        "hcr_accept:{}:{}:{}",
        hcr_id, outcome.claim_id.0, outcome.run_id.0
    );

    let intent = InvocationIntent {
        invocation_id: invocation_id.clone(),
        run_id: outcome.run_id.clone(),
        operation: "external.coding_hcr_accept".into(),
        arguments: json!({
            "candidate_ref": candidate_ref,
            "hcr_id": hcr_id,
            "claim_id": outcome.claim_id.0,
            "run_id": outcome.run_id.0,
        }),
        idempotency_key: Some(idempotency_key.clone()),
    };

    let approved = gateway.approve_invocation(intent.clone(), &run, &session, &snapshot)?;

    // 5. Call Coding Harness HTTP endpoint
    let harness_response = call_harness_accept(
        config,
        &approved,
        &outcome.claim_id.0,
        &outcome.run_id.0,
        candidate_ref,
    )?;

    // 6. Determine receipt status and outcome
    let outcome_str = harness_response
        .get("result")
        .and_then(|r| r.get("outcome"))
        .and_then(|v| v.as_str())
        .unwrap_or("InfrastructureFailure");

    let receipt_status = match outcome_str {
        "CandidatePassed" => ReceiptStatus::Succeeded,
        "CandidateFailed" => ReceiptStatus::Failed,
        _ => ReceiptStatus::Failed,
    };

    // 7. Write ReceiptReceived event
    let now = Utc::now();
    let receipt_event_id = EventId::new();
    let receipt_payload = json!({
        "invocation_id": invocation_id.0,
        "status": format!("{:?}", receipt_status),
        "output": harness_response.get("result"),
        "hcr_id": hcr_id,
        "claim_id": outcome.claim_id.0,
        "run_id": outcome.run_id.0,
        "idempotency_key": idempotency_key,
        "operation": "external.coding_hcr_accept",
    });

    journal.append_event(
        JournalEventKind::ReceiptReceived,
        Some(&outcome.run_id),
        Some(&session.id),
        Some(&idempotency_key),
        receipt_payload,
    )?;

    // 8. Call R3A settlement
    let settlement_result =
        settle_hcr(journal, hcr_id, &outcome.claim_id.0, &outcome.run_id.0)?;

    Ok(json!({
        "ok": true,
        "hcr_id": hcr_id,
        "claim_id": outcome.claim_id.0,
        "run_id": outcome.run_id.0,
        "outcome": outcome_str,
        "receipt_status": format!("{:?}", receipt_status),
        "settlement_result": format!("{:?}", settlement_result),
        "harness_response": harness_response,
    }))
}

/// Call the Coding Harness acceptance endpoint over HTTP.
fn call_harness_accept(
    config: &KernelConfig,
    approved: &ApprovedInvocation,
    _claim_id: &str,
    _run_id: &str,
    candidate_ref: &str,
) -> Result<Value> {
    let harness_url = "http://127.0.0.1:7200";

    let body = json!({
        "protocol_version": "external-harness-v1",
        "operation": "external.coding_hcr_accept",
        "arguments": {
            "candidate_ref": candidate_ref,
            "hcr_id": approved.intent().arguments.get("hcr_id"),
            "claim_id": approved.intent().arguments.get("claim_id"),
            "run_id": approved.intent().arguments.get("run_id"),
        },
    });

    let body_str = serde_json::to_string(&body)?;
    let request = format!(
        "POST /execute HTTP/1.1\r\nHost: 127.0.0.1:7200\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str,
    );

    let mut stream = TcpStream::connect("127.0.0.1:7200")?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(300)))?;
    stream.write_all(request.as_bytes())?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;

    let response = String::from_utf8_lossy(&buf);
    let json_body: Value = if let Some(body_start) = response.find("\r\n\r\n") {
        let body = &response[body_start + 4..];
        serde_json::from_str(body).unwrap_or(json!({"parse_error": body.to_string()}))
    } else {
        json!({"parse_error": "no_body"})
    };

    Ok(json_body)
}

impl JournalStore {
    /// Create a persisted Run record.
    pub fn create_hcr_run(&self, run: &Run) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("mutex: {e}"))?;
        let mode_str = serde_json::to_string(&run.mode)?;
        conn.execute(
            "INSERT OR IGNORE INTO runs (id, session_id, agent_id, trigger_event_id, principal_json,
             parent_run_id, delegated_by, status, created_at, updated_at, registry_snapshot_id, mode)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                run.id.0, run.session_id.0, run.agent_id.0, run.trigger_event_id.0,
                serde_json::to_string(&run.principal)?,
                run.parent_run_id.as_ref().map(|r| r.0.as_str()),
                run.delegated_by.as_ref().map(|p| p.0.as_str()),
                format!("{:?}", run.status),
                run.created_at.to_rfc3339(),
                run.updated_at.to_rfc3339(),
                run.registry_snapshot_id,
                mode_str,
            ],
        )?;
        Ok(())
    }
}
