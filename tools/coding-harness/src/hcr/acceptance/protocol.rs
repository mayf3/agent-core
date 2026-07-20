//! Acceptance protocol types, fingerprint, and evidence digest.
//!
//! Defines the structured request/response contract between Kernel
//! and Coding Harness for HCR candidate acceptance.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Canonical request fingerprint for idempotency.
///
/// Generated from all identity fields and the candidate reference.
/// Two requests with the same fingerprint are semantically identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFingerprint(pub String);

/// Compute a canonical request fingerprint.
///
/// Serializes all identity fields to a canonical JSON object and
/// computes its SHA-256 digest. Used by the execution store to
/// detect conflicting replay (same idempotency_key, different request).
pub fn compute_fingerprint(
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    principal_id: &str,
    gateway_session_id: &str,
    registry_snapshot_id: &str,
    operation: &str,
    candidate_ref: &str,
    idempotency_key: &str,
    requirement_digest: Option<&str>,
) -> RequestFingerprint {
    let canonical = serde_json::json!({
        "hcr_id": hcr_id,
        "claim_id": claim_id,
        "run_id": run_id,
        "principal_id": principal_id,
        "gateway_session_id": gateway_session_id,
        "registry_snapshot_id": registry_snapshot_id,
        "operation": operation,
        "candidate_ref": candidate_ref,
        "idempotency_key": idempotency_key,
        "requirement_digest": requirement_digest,
    });
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let hex = hex::encode(Sha256::digest(&bytes));
    RequestFingerprint(format!("sha256:{hex}"))
}

/// Structured acceptance response sent from Harness to Kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceResponse {
    pub harness_execution_id: String,
    pub idempotency_key: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub principal_id: String,
    pub gateway_session_id: String,
    pub registry_snapshot_id: String,
    pub operation: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub overall_outcome: String,
    pub gate_results: Vec<GateResultEntry>,
    pub artifact_ref: Option<String>,
    pub artifact_digest: Option<String>,
    pub component_manifest_digest: Option<String>,
    /// Content‑addressable ref of the delivery manifest (e.g.
    /// `"service_manifest_<sha256>"`).  Set only for `CandidatePassed`.
    pub delivery_manifest_ref: Option<String>,
    /// ContentStore digest of the delivery manifest bytes
    /// (`"sha256:<hex>"`).  Set only for `CandidatePassed`.
    pub delivery_manifest_digest: Option<String>,
    pub evidence_digest: String,
}

/// A single gate result entry in the acceptance response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResultEntry {
    pub gate_kind: String,
    pub passed: bool,
    pub is_candidate_failure: bool,
    pub exit_code: i32,
    pub timed_out: bool,
    pub error_code: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

/// Compute the canonical evidence digest for an acceptance response.
///
/// The evidence document includes:
/// - harness_execution_id
/// - request fingerprint
/// - candidate_id, candidate_digest
/// - all five gate results
/// - overall_outcome
/// - artifact_ref, artifact_digest
pub fn compute_evidence_digest(
    harness_execution_id: &str,
    fingerprint: &RequestFingerprint,
    candidate_id: &str,
    candidate_digest: &str,
    gate_results: &[GateResultEntry],
    overall_outcome: &str,
    artifact_ref: Option<&str>,
    artifact_digest: Option<&str>,
    component_manifest_digest: Option<&str>,
) -> String {
    let bytes = canonical_evidence_bytes(
        harness_execution_id,
        fingerprint,
        candidate_id,
        candidate_digest,
        gate_results,
        overall_outcome,
        artifact_ref,
        artifact_digest,
        component_manifest_digest,
    );
    let hex = hex::encode(Sha256::digest(&bytes));
    format!("sha256:{hex}")
}

/// Canonical evidence bytes stored in the shared content-addressed store.
pub fn canonical_evidence_bytes(
    harness_execution_id: &str,
    fingerprint: &RequestFingerprint,
    candidate_id: &str,
    candidate_digest: &str,
    gate_results: &[GateResultEntry],
    overall_outcome: &str,
    artifact_ref: Option<&str>,
    artifact_digest: Option<&str>,
    component_manifest_digest: Option<&str>,
) -> Vec<u8> {
    let evidence = serde_json::json!({
        "harness_execution_id": harness_execution_id,
        "request_fingerprint": fingerprint.0,
        "candidate_id": candidate_id,
        "candidate_digest": candidate_digest,
        "gate_results": gate_results,
        "overall_outcome": overall_outcome,
        "artifact_ref": artifact_ref,
        "artifact_digest": artifact_digest,
        "component_manifest_digest": component_manifest_digest,
    });
    serde_json::to_vec(&evidence).unwrap_or_default()
}

/// Sanitize an idempotency key for use as a filesystem path component.
/// Uses SHA-256 to prevent path traversal.
pub fn sanitize_key(key: &str) -> String {
    let hex = hex::encode(Sha256::digest(key.as_bytes()));
    hex[..16].to_string()
}
