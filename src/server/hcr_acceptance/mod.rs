//! Internal HCR acceptance trigger.
//!
//! `POST /v1/hcr/:hcr_id/accept`
//!
//! Orchestrates the full HCR acceptance flow with strict response
//! validation (H2), atomic receipt append-or-compare (H3), and
//! real artifact/evidence digest handling (H4/H5).

pub mod receipt;
pub mod response_validation;

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::hcr::{settlement::settle_hcr, worker::execute_hcr};
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use receipt::AppendReceiptResult;
use response_validation::{validate_harness_response, RequestContext};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;

/// Handle a POST /v1/hcr/:hcr_id/accept request.
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

    // 2. Create/get session
    let session_target = SessionTarget {
        agent_id: AgentId::new(),
        channel: ChannelKind::Cli,
        conversation_key: format!("hcr-accept-{hcr_id}"),
    };
    let session = journal.get_or_create_session(&session_target)?;

    // 3. Create Run with RunMode::Hcr
    let trigger_event_id = EventId::new();
    let harness_id = {
        let hcr = journal.get_harness_change_request(hcr_id)?
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
        trigger_event_id,
        principal: principal.clone(),
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: snapshot_id.clone(),
        mode: RunMode::Hcr {
            hcr_id: hcr_id.to_string(),
            harness_id,
            claim_id: outcome.claim_id.0.clone(),
        },
    };
    journal.create_hcr_run(&run)?;

    // 4. Gateway approval
    let invocation_id = InvocationId::new();
    let idempotency_key = format!("hcr_accept:{}:{}:{}", hcr_id, outcome.claim_id.0, outcome.run_id.0);
    let intent = InvocationIntent {
        invocation_id,
        run_id: outcome.run_id.clone(),
        operation: "external.coding_hcr_accept".into(),
        arguments: json!({
            "candidate_ref": candidate_ref,
            "hcr_id": hcr_id,
            "claim_id": outcome.claim_id.0,
            "run_id": outcome.run_id.0,
            "principal_id": principal.principal_id.0,
            "gateway_session_id": session.id.0,
            "registry_snapshot_id": snapshot_id,
            "idempotency_key": idempotency_key,
        }),
        idempotency_key: Some(idempotency_key.clone()),
    };
    let approved = gateway.approve_invocation(intent.clone(), &run, &session, &snapshot)?;

    // 5. Call Harness
    let harness_response = call_harness_accept(config, &approved, candidate_ref, &idempotency_key,
        hcr_id, &outcome.claim_id.0, &outcome.run_id.0,
        &principal.principal_id.0, &session.id.0, &snapshot_id)?;

    // 6. Validate Harness response (H2)
    let ctx = RequestContext {
        hcr_id: hcr_id.to_string(),
        claim_id: outcome.claim_id.0.clone(),
        run_id: outcome.run_id.0.clone(),
        principal_id: principal.principal_id.0.clone(),
        gateway_session_id: session.id.0.clone(),
        registry_snapshot_id: snapshot_id.clone(),
        operation: "external.coding_hcr_accept".into(),
        idempotency_key: idempotency_key.clone(),
    };
    let validated = match validate_harness_response(&harness_response, &ctx) {
        Ok(v) => v,
        Err(e) => bail!("RESPONSE_VALIDATION_FAILED: {e}"),
    };

    // 7. Determine receipt status
    let receipt_status = match validated.overall_outcome.as_str() {
        "CandidatePassed" => ReceiptStatus::Succeeded,
        _ => ReceiptStatus::Failed,
    };

    let output = json!({
        "harness_execution_id": validated.harness_execution_id,
        "overall_outcome": validated.overall_outcome,
        "candidate_digest": validated.candidate_digest,
        "artifact_digest": validated.artifact_digest,
        "evidence_digest": validated.evidence_digest,
        "gate_count": validated.gate_count,
    });

    // 8. Atomic receipt append-or-compare (H3)
    let receipt_key_str = receipt::receipt_key(
        hcr_id, &outcome.claim_id.0, &outcome.run_id.0, &idempotency_key
    );
    match receipt::append_or_compare_receipt(
        journal, &outcome.run_id, &session.id,
        &receipt_key_str, receipt_status, &output,
    )? {
        AppendReceiptResult::Appended => {}
        AppendReceiptResult::Duplicate => {
            // Idempotent replay — skip settlement, return existing
            return Ok(json!({
                "ok": true, "status": "duplicate",
                "hcr_id": hcr_id, "outcome": validated.overall_outcome,
            }));
        }
        AppendReceiptResult::Conflict(msg) => {
            bail!("RECEIPT_CONFLICT: {msg}");
        }
    }

    // 9. R3A settlement
    let settlement = settle_hcr(journal, hcr_id, &outcome.claim_id.0, &outcome.run_id.0)?;

    Ok(json!({
        "ok": true,
        "hcr_id": hcr_id,
        "claim_id": outcome.claim_id.0,
        "run_id": outcome.run_id.0,
        "outcome": validated.overall_outcome,
        "harness_execution_id": validated.harness_execution_id,
        "artifact_digest": validated.artifact_digest,
        "evidence_digest": validated.evidence_digest,
        "settlement_result": format!("{:?}", settlement),
    }))
}

fn call_harness_accept(
    config: &KernelConfig,
    approved: &ApprovedInvocation,
    candidate_ref: &str,
    idempotency_key: &str,
    hcr_id: &str, claim_id: &str, run_id: &str,
    principal_id: &str, gateway_session_id: &str, registry_snapshot_id: &str,
) -> Result<Value> {
    let body = json!({
        "protocol_version": "external-harness-v1",
        "operation": "external.coding_hcr_accept",
        "arguments": {
            "candidate_ref": candidate_ref,
            "hcr_id": hcr_id,
            "claim_id": claim_id,
            "run_id": run_id,
            "principal_id": principal_id,
            "gateway_session_id": gateway_session_id,
            "registry_snapshot_id": registry_snapshot_id,
            "operation": "external.coding_hcr_accept",
            "idempotency_key": idempotency_key,
        },
    });

    let body_str = serde_json::to_string(&body)?;
    let request = format!(
        "POST /execute HTTP/1.1\r\nHost: 127.0.0.1:7200\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(), body_str,
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
