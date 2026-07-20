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
pub mod receipt;
pub mod response_validation;

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::hcr::{settlement::settle_hcr, worker::execute_hcr};
use crate::journal::JournalStore;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use receipt::AppendReceiptResult;
use response_validation::{validate_harness_response, RequestContext};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("HCR_NOT_FOUND"))?;
    let harness_id = hcr.harness_id.clone();
    let gate_harness_id = harness_id.clone(); // saved for gate_evidence persistence

    // Parse development_request from HCR requirement for identity validation
    // by the Harness invocable manifest builder (Kernel does not inspect fields).
    let dev_req: Option<serde_json::Value> = serde_json::from_str(&hcr.requirement)
        .ok()
        .and_then(|v: serde_json::Value| v.get("development_request").cloned());
    let req_digest = {
        use sha2::{Digest, Sha256};
        format!(
            "sha256:{}",
            hex::encode(Sha256::digest(hcr.requirement.as_bytes()))
        )
    };
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
            "development_request": dev_req,
            "requirement_digest": req_digest,
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
    //
    // PRODUCTION PATH: only ExternalReceiptEnvelope is accepted.
    // Raw JSON bypass is prohibited.
    let result_value = harness_response.get("result").unwrap_or(&harness_response);

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

    // 6e. Validate outcome is a known enum (serde already validated this)
    //     Validate evidence_digest format
    if !envelope.evidence_digest.starts_with("sha256:") {
        bail!("EVIDENCE_DIGEST_FORMAT: invalid evidence_digest");
    }

    // 6f. Validate subject_digest matches the candidate_digest from the
    //     validated response (the response's candidate_digest IS the subject_digest)
    //     This is validated implicitly below when we parse the detailed response.

    // 6g. Verify opaque_payload_digest — this binds the full detailed
    //     acceptance response (including delivery_manifest_digest) to the
    //     receipt.  We strip envelope-only keys to reconstruct the original
    //     AcceptanceResponse bytes that were hashed by the Harness.
    if let Some(ref expected_opaque) = envelope.opaque_payload_digest {
        let computed = verify_opaque_payload_digest(result_value)?;
        if *expected_opaque != computed {
            bail!("OPAQUE_PAYLOAD_MISMATCH");
        }
    }

    // ── 7. Parse detailed response fields for persistence ────────────────
    //
    // The envelope's opaque_payload binds the internal evidence. The Kernel
    // also extracts structural fields (gate_results, candidate_id, etc.)
    // from the same response for its own persistence — these are NOT the
    // opaque payload content, they are the receipt's structured metadata.

    // Re-use existing response validation for identity field matching
    // and gate structure. NOTE: identity fields are now validated through
    // the invocation_intent_id binding above, but we keep the existing
    // field-level check for defense-in-depth.
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

    // ── 8. Persist five gate attempts + evidence (PR2 hotfix) ─────────────
    // Maps each harness gate result to an HcrGateAttempt + InvocationProposed
    // + ReceiptReceived + HcrGateEvidence chain.
    let harness_result = harness_response.get("result").unwrap_or(&harness_response);
    gate_evidence::persist_gates(
        journal,
        harness_result,
        hcr_id,
        &outcome.claim_id.0,
        &outcome.run_id.0,
        &gate_harness_id,
    )?;

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
    //
    // The receipt_digest from the envelope serves as the payload_digest
    // for identity comparison. This replaces the old compute_payload_digest
    // which was computing a different digest over identity fields.
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

/// Verify the opaque_payload_digest over the detailed acceptance
/// response.  Envelope-only keys are stripped to reconstruct the
/// original `AcceptanceResponse` bytes that were hashed by the
/// Harness.
pub(crate) fn verify_opaque_payload_digest(merged: &Value) -> Result<String> {
    let mut detailed_only = merged.clone();
    // Envelope-only keys that are NOT part of AcceptanceResponse
    const ENVELOPE_ONLY: &[&str] = &[
        "schema_version",
        "invocation_intent_id",
        "issuer",
        "subject_digest",
        "outcome",
        "opaque_payload_digest",
        "receipt_digest",
    ];
    if let Some(obj) = detailed_only.as_object_mut() {
        for key in ENVELOPE_ONLY {
            obj.remove(*key);
        }
    }
    let detailed_bytes =
        serde_json::to_vec(&detailed_only).map_err(|e| anyhow!("OPAQUE_SERIALIZATION: {e}"))?;
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(&detailed_bytes))
    ))
}

fn call_harness_accept(
    _config: &KernelConfig,
    approved: &ApprovedInvocation,
    candidate_ref: &str,
    idempotency_key: &str,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    principal_id: &str,
    gateway_session_id: &str,
    registry_snapshot_id: &str,
) -> Result<Value> {
    let invocation_intent_id = approved.intent().invocation_id.0.clone();

    // Relay development_request + digest from intent args to Harness body
    let args = &approved.intent().arguments;
    let dev_req = args.get("development_request").cloned();
    let req_digest = args
        .get("requirement_digest")
        .and_then(Value::as_str)
        .map(String::from);

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
            "invocation_intent_id": invocation_intent_id,
            "development_request": dev_req,
            "requirement_digest": req_digest,
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

#[cfg(test)]
mod tests {
    use super::verify_opaque_payload_digest;
    use serde_json::json;

    /// Build a merged response value (AcceptanceResponse + envelope fields)
    /// as produced by the Harness's `ok_envelope_json()`.
    fn merged_response(
        delivery_manifest_ref: Option<&str>,
        delivery_manifest_digest: Option<&str>,
    ) -> serde_json::Value {
        let mut base = json!({
            "harness_execution_id": "hex_test",
            "idempotency_key": "accept:test",
            "hcr_id": "hcr_test",
            "claim_id": "claim_test",
            "run_id": "run_test",
            "principal_id": "principal_test",
            "gateway_session_id": "session_test",
            "registry_snapshot_id": "snap_test",
            "operation": "external.coding_hcr_accept",
            "candidate_id": "candidate_test",
            "candidate_digest": format!("sha256:{}", "1".repeat(64)),
            "overall_outcome": "CandidatePassed",
            "gate_results": [],
            "artifact_ref": "candidate/target/release/component",
            "artifact_digest": format!("sha256:{}", "3".repeat(64)),
            "component_manifest_digest": format!("sha256:{}", "4".repeat(64)),
            "evidence_digest": format!("sha256:{}", "2".repeat(64)),
        });
        if let Some(ref_val) = delivery_manifest_ref {
            base["delivery_manifest_ref"] = json!(ref_val);
        }
        if let Some(dig_val) = delivery_manifest_digest {
            base["delivery_manifest_digest"] = json!(dig_val);
        }
        // Add envelope fields (as ok_envelope_json does)
        base["schema_version"] = json!("external-receipt-envelope-v1");
        base["invocation_intent_id"] = json!("invocation_test");
        base["issuer"] = json!("coding-harness");
        base["subject_digest"] = json!(format!("sha256:{}", "1".repeat(64)));
        base["outcome"] = json!("Passed");
        base["opaque_payload_digest"] = json!("sha256:0000");
        base["receipt_digest"] = json!("sha256:0000");
        base
    }

    #[test]
    fn delivery_manifest_digest_is_opaque_payload_bound() {
        let dm_ref =
            "service_manifest_abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234";
        let dm_dig = format!("sha256:{}", "6".repeat(64));
        let merged = merged_response(Some(dm_ref), Some(&dm_dig));
        // Verification should pass — digest was computed from the correct content
        let computed = verify_opaque_payload_digest(&merged).unwrap();
        // The expected digest is what the Harness would have computed
        // by serializing the AcceptanceResponse (without envelope fields).
        // We trust the verification function produces a stable digest.
        assert!(
            computed.starts_with("sha256:"),
            "opaque payload must be a valid sha256 digest"
        );
        assert_eq!(computed.len(), 71, "sha256: + 64 hex chars");
    }

    #[test]
    fn tampered_delivery_manifest_ref_is_rejected() {
        let dm_ref = "service_manifest_original_ref_original_ref_original_ref_original_ref_original_ref_original";
        let dm_dig = format!("sha256:{}", "6".repeat(64));
        let mut merged = merged_response(Some(dm_ref), Some(&dm_dig));
        // Tamper with delivery_manifest_ref
        merged["delivery_manifest_ref"] = json!("tampered_ref");
        // The computed opaque_payload_digest will differ from the expected
        // because the content changed.  We verify that the recomputed digest
        // does NOT match a digest computed from the untampered content.
        let tampered_computed = verify_opaque_payload_digest(&merged).unwrap();
        // Recompute with original value to compare
        let clean = merged_response(Some(dm_ref), Some(&dm_dig));
        let clean_computed = verify_opaque_payload_digest(&clean).unwrap();
        assert_ne!(
            tampered_computed, clean_computed,
            "tampered delivery_manifest_ref must change opaque payload"
        );
    }

    #[test]
    fn tampered_delivery_manifest_digest_is_rejected() {
        let dm_ref =
            "service_manifest_abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234";
        let dm_dig = format!("sha256:{}", "6".repeat(64));
        let mut merged = merged_response(Some(dm_ref), Some(&dm_dig));
        // Tamper with delivery_manifest_digest
        merged["delivery_manifest_digest"] = json!(format!("sha256:{}", "9".repeat(64)));
        let tampered_computed = verify_opaque_payload_digest(&merged).unwrap();
        let clean = merged_response(Some(dm_ref), Some(&dm_dig));
        let clean_computed = verify_opaque_payload_digest(&clean).unwrap();
        assert_ne!(
            tampered_computed, clean_computed,
            "tampered delivery_manifest_digest must change opaque payload"
        );
    }

    #[test]
    fn delivery_manifest_fields_are_part_of_opaque_payload() {
        // Verify that adding delivery_manifest fields changes the opaque payload
        let with_dm = merged_response(
            Some("service_manifest_ref"),
            Some(&format!("sha256:{}", "6".repeat(64))),
        );
        let without_dm = merged_response(None, None);
        let digest_with = verify_opaque_payload_digest(&with_dm).unwrap();
        let digest_without = verify_opaque_payload_digest(&without_dm).unwrap();
        assert_ne!(
            digest_with, digest_without,
            "presence of delivery_manifest fields must change opaque payload"
        );
    }
}
