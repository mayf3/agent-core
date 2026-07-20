//! Low-level HTTP transport to the Coding Harness for HCR acceptance.
//!
//! Runs inside the Kernel process.  Sends the approved invocation as a
//! raw HTTP/1.1 request to the local Coding Harness and parses the
//! JSON response.  All higher-level validation (envelope, receipt, etc.)
//! is handled by the caller in `mod.rs`.

use crate::config::KernelConfig;
use crate::domain::*;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;

/// Send the approved HCR acceptance invocation to the Coding Harness
/// and return the raw JSON response.
///
/// The Harness must be listening on `127.0.0.1:7200`.  The request
/// includes the candidate reference, identity fields, the opaque
/// development_request, and the requirement_digest — all forwarded
/// from the InvocationIntent arguments without Kernel inspection.
pub fn call_harness_accept(
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
    let raw_requirement = args
        .get("requirement")
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
            "requirement": raw_requirement,
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

/// Recompute the opaque_payload_digest from a merged response value
/// (AcceptanceResponse + envelope fields).  Envelope-only keys are
/// stripped to reconstruct the original `AcceptanceResponse` bytes
/// that were hashed by the Harness.
pub fn verify_opaque_payload_digest(merged: &Value) -> Result<String> {
    let mut detailed_only = merged.clone();
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
