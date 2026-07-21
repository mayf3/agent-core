//! Internal HCR acceptance trigger.
//!
//! `POST /v1/hcr/:hcr_id/accept`
//!
//! Orchestrates the full HCR acceptance flow with strict envelope
//! validation (H1), mechanical ExternalReceiptEnvelope validation (H2),
//! and atomic receipt append-or-compare (H3/H6).
//!
//! The production path ONLY accepts ExternalReceiptEnvelope from the
//! Coding Harness. Raw JSON bypass is prohibited.

pub mod gate_evidence;
pub mod harness_client;
pub mod receipt;
pub mod request_binding;
pub mod response_validation;

#[cfg(test)]
pub mod tests;

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::hcr::{settlement::settle_hcr, worker::execute_hcr};
use crate::journal::JournalStore;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use harness_client::call_harness_accept;
use receipt::AppendReceiptResult;
use request_binding::build_requirement_binding;
use response_validation::{validate_harness_response, RequestContext};
use serde_json::{json, Value};

/// Handle a POST /v1/hcr/:hcr_id/accept request.
pub fn handle(
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    hcr_id: &str,
    body: &Value,
) -> Result<Value> {
    let result = handle_inner(journal, gateway, config, hcr_id, body);
    if let Err(ref e) = result {
        eprintln!("[HCR_ACCEPT] handle failed for hcr_id={hcr_id}: {e}");
    }
    result
}

fn handle_inner(
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

    // 2. Create/get session — use the HCR's principal context (Feishu p2p)
    //    so the proposal origin context is correct for the connector's
    //    Feishu/p2p approval binding check.
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("HCR_NOT_FOUND"))?;
    let conversation_key = if hcr.channel.eq_ignore_ascii_case("Feishu") {
        hcr.principal_id.clone()
    } else {
        format!("hcr-accept-{hcr_id}")
    };
    let session_target = SessionTarget {
        agent_id: AgentId::new(),
        channel: if hcr.channel.eq_ignore_ascii_case("Feishu") {
            ChannelKind::Feishu
        } else {
            ChannelKind::Cli
        },
        conversation_key,
    };
    let session = journal.get_or_create_session(&session_target)?;

    // 3. Create Run with RunMode::Hcr
    let trigger_event_id = EventId::new();
    // hcr was loaded in step 2 above
    let harness_id = hcr.harness_id.clone();
    let gate_harness_id = harness_id.clone();

    // Extract requirement binding (opaque — Kernel does not inspect fields)
    let binding = build_requirement_binding(&hcr.requirement);
    let principal = RunPrincipal {
        principal_id: PrincipalId(hcr.principal_id.clone()),
        subject: if let Some(open_id) = hcr.principal_id.strip_prefix("feishu:open_id:") {
            PrincipalSubject::FeishuOpenId(open_id.to_string())
        } else {
            PrincipalSubject::LocalUser
        },
        source: if hcr.channel.eq_ignore_ascii_case("feishu") {
            PrincipalSource::Feishu
        } else {
            PrincipalSource::Cli
        },
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
    let idempotency_key = format!(
        "hcr_accept:{}:{}:{}",
        hcr_id, outcome.claim_id.0, outcome.run_id.0
    );
    let intent = InvocationIntent {
        invocation_id: invocation_id.clone(),
        run_id: outcome.run_id.clone(),
        operation: "external.coding_hcr_accept".into(),
        arguments: json!({
            "session_id": session.id.0,
            "candidate_ref": candidate_ref,
            "hcr_id": hcr_id,
            "claim_id": outcome.claim_id.0,
            "run_id": outcome.run_id.0,
            "principal_id": principal.principal_id.0,
            "gateway_session_id": session.id.0,
            "registry_snapshot_id": snapshot_id,
            "idempotency_key": idempotency_key,
            "development_request": binding.development_request,
            "requirement_digest": binding.requirement_digest,
            "requirement": hcr.requirement,
        }),
        idempotency_key: Some(idempotency_key.clone()),
    };
    let approved = gateway.approve_invocation(intent.clone(), &run, &session, &snapshot)?;

    // 5. Call Harness — passes invocation_intent_id for envelope binding
    let harness_response = call_harness_accept(
        config,
        &approved,
        candidate_ref,
        &idempotency_key,
        hcr_id,
        &outcome.claim_id.0,
        &outcome.run_id.0,
        &principal.principal_id.0,
        &session.id.0,
        &snapshot_id,
    )?;

    // ── 6. ExternalReceiptEnvelope validation (H1/H2) ─────────────────────
    let result_value = harness_response.get("result").unwrap_or(&harness_response);

    // 6z. Check for error responses from the coding harness (e.g. MISSING_TARGET_KIND)
    if result_value.get("error_code").and_then(Value::as_str).is_some()
        || result_value.get("error").and_then(Value::as_str).is_some()
        || harness_response.get("ok").and_then(Value::as_bool) == Some(false)
    {
        let error_code = result_value
            .get("error_code")
            .and_then(Value::as_str)
            .or_else(|| result_value.get("error").and_then(Value::as_str))
            .or_else(|| harness_response.get("error_code").and_then(Value::as_str))
            .unwrap_or("HARNESS_ACCEPTANCE_FAILED");
        bail!("HARNESS_ACCEPTANCE_FAILED: {error_code}");
    }

    let envelope: ExternalReceiptEnvelope = match serde_json::from_value(result_value.clone()) {
        Ok(env) => env,
        Err(e) => {
            bail!("ENVELOPE_DESERIALIZATION_FAILED: {e}");
        }
    };

    // 6a. Validate envelope structure
    if let Err(e) = envelope.validate_structure() {
        bail!("ENVELOPE_VALIDATION_FAILED: {e}");
    }

    // 6b. Independently recompute receipt_digest
    if let Err(e) = envelope.verify_receipt_digest() {
        bail!("RECEIPT_DIGEST_MISMATCH: {e}");
    }

    // 6c. Validate InvocationIntent binding
    if envelope.invocation_intent_id != invocation_id.0 {
        bail!(
            "INVOCATION_MISMATCH: envelope has '{}', kernel has '{}'",
            envelope.invocation_intent_id,
            invocation_id.0
        );
    }

    // 6d. Validate issuer
    if envelope.issuer != "coding-harness" {
        bail!(
            "ISSUER_MISMATCH: expected 'coding-harness', got '{}'",
            envelope.issuer
        );
    }

    // 6e. Validate evidence_digest format
    if !envelope.evidence_digest.starts_with("sha256:") {
        bail!("EVIDENCE_DIGEST_FORMAT: invalid evidence_digest");
    }

    // 6f. Verify opaque_payload_digest — binds the full detailed acceptance
    //     response (including delivery_manifest_digest) to the receipt.
    if let Some(ref expected_opaque) = envelope.opaque_payload_digest {
        let computed = harness_client::verify_opaque_payload_digest(result_value)?;
        if *expected_opaque != computed {
            bail!("OPAQUE_PAYLOAD_MISMATCH");
        }
    }

    // ── 7. Parse detailed response fields for persistence ────────────────
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

    // ── 8. Persist five gate attempts + evidence ─────────────────────────
    let harness_result = harness_response.get("result").unwrap_or(&harness_response);
    eprintln!("[HCR_ACCEPT_DEBUG] step 8: persist_gates start");
    gate_evidence::persist_gates(
        journal,
        harness_result,
        hcr_id,
        &outcome.claim_id.0,
        &outcome.run_id.0,
        &gate_harness_id,
    )?;
    eprintln!("[HCR_ACCEPT_DEBUG] step 8: persist_gates done");

    // 9. Determine receipt status from envelope
    let receipt_status = match envelope.outcome {
        external_receipt_envelope::ExternalOutcome::Passed => ReceiptStatus::Succeeded,
        external_receipt_envelope::ExternalOutcome::Failed => ReceiptStatus::Failed,
    };

    let output = json!({
        "harness_execution_id": validated.harness_execution_id,
        "overall_outcome": validated.overall_outcome,
        "candidate_id": validated.candidate_id,
        "candidate_digest": validated.candidate_digest,
        "artifact_ref": validated.artifact_ref,
        "artifact_digest": validated.artifact_digest,
        "component_manifest_digest": validated.component_manifest_digest,
        "delivery_manifest_ref": validated.delivery_manifest_ref,
        "delivery_manifest_digest": validated.delivery_manifest_digest,
        "evidence_digest": validated.evidence_digest,
        "gate_count": validated.gate_count,
    });

    // 10. Atomic receipt append-or-compare (H3/H6)
    eprintln!("[HCR_ACCEPT_DEBUG] step 10: append_or_compare_receipt start");
    let identity_fields = receipt::ReceiptIdentityFields {
        harness_execution_id: validated.harness_execution_id.clone(),
        overall_outcome: validated.overall_outcome.clone(),
        candidate_id: validated.candidate_id.clone(),
        invocation_id: invocation_id.0.clone(),
        candidate_digest: validated.candidate_digest.clone(),
        artifact_ref: validated.artifact_ref.clone(),
        artifact_digest: validated.artifact_digest.clone(),
        delivery_manifest_ref: validated.delivery_manifest_ref.clone(),
        delivery_manifest_digest: validated.delivery_manifest_digest.clone(),
        evidence_digest: envelope.evidence_digest.clone(),
        receipt_digest: envelope.receipt_digest.clone(),
        opaque_payload_digest: envelope.opaque_payload_digest.clone(),
    };
    match receipt::append_or_compare_receipt(
        journal,
        &outcome.run_id,
        &session.id,
        hcr_id,
        &outcome.claim_id.0,
        &outcome.run_id.0,
        &idempotency_key,
        receipt_status,
        &output,
        &envelope.receipt_digest,
        &identity_fields,
    )? {
        AppendReceiptResult::Appended => {}
        AppendReceiptResult::Duplicate => {
            return Ok(json!({
                "ok": true, "status": "duplicate",
                "hcr_id": hcr_id, "outcome": validated.overall_outcome,
            }));
        }
        AppendReceiptResult::Conflict(msg) => {
            bail!("RECEIPT_CONFLICT: {msg}");
        }
    }

    // 11. R3A settlement
    let settlement = settle_hcr(journal, hcr_id, &outcome.claim_id.0, &outcome.run_id.0)?;
    let settlement_id = match &settlement {
        SettlementResult::Succeeded(id) | SettlementResult::CandidateFailed(id) => id.clone(),
        _ => String::new(),
    };

    Ok(json!({
        "ok": true,
        "acceptance_invocation_id": invocation_id.0,
        "hcr_id": hcr_id,
        "claim_id": outcome.claim_id.0,
        "run_id": outcome.run_id.0,
        "outcome": validated.overall_outcome,
        "harness_execution_id": validated.harness_execution_id,
        "candidate_id": validated.candidate_id,
        "candidate_digest": validated.candidate_digest,
        "artifact_ref": validated.artifact_ref,
        "artifact_digest": validated.artifact_digest,
        "component_manifest_digest": validated.component_manifest_digest,
        "delivery_manifest_ref": validated.delivery_manifest_ref,
        "delivery_manifest_digest": validated.delivery_manifest_digest,
        "evidence_digest": validated.evidence_digest,
        "settlement_id": settlement_id,
        "settlement_result": format!("{:?}", settlement),
    }))
}
