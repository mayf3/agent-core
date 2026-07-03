//! Capability proposal support for coding.capability.propose.
//!
//! Uses the real `HarnessManifest` type, `Sha256Digest` for content-addressed
//! digest computation, `ContentStore` for blob storage, and the existing
//! `handle_submit_proposal` API to create proposals.
//!
//! The Coding Harness does NOT hold or call decision tokens.

use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::AgentId;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_submit_proposal;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::Path;

/// Maximum size for manifest and evidence files.
const MAX_MANIFEST_SIZE: usize = 256 * 1024;
const MAX_EVIDENCE_SIZE: usize = 256 * 1024;

fn ok(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
fn err(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}

/// Handle a capability proposal request.
///
/// Args (from request JSON):
/// - artifact_path: relative path to the artifact binary
/// - manifest_path: relative path to the manifest JSON (optionally partial)
/// - evidence_path: relative path to the evidence JSON
/// - target_agent_id: the agent ID to target (default "main")
/// - origin_session_id: session ID for the proposal
/// - origin_run_id: run ID for the proposal
/// - risk_summary: description of risk (default "read-only")
///
/// The manifest JSON at manifest_path is read and parsed as a (possibly
/// partial) HarnessManifest. The artifact_digest and manifest_id fields are
/// then computed and set before storage.
pub fn handle_propose(
    root: &Path,
    args: &Value,
    journal: &JournalStore,
    gateway: &Gateway,
    store: &ContentStore,
    config_agent_id: &AgentId,
) -> Value {
    let artifact_rel = args
        .get("artifact_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let manifest_rel = args
        .get("manifest_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let evidence_rel = args
        .get("evidence_path")
        .and_then(Value::as_str)
        .unwrap_or("");

    if artifact_rel.is_empty() || manifest_rel.is_empty() || evidence_rel.is_empty() {
        return err("missing_path");
    }

    let artifact_path = root.join(artifact_rel);
    let manifest_path = root.join(manifest_rel);
    let evidence_path = root.join(evidence_rel);

    // ── Read all three files ──
    let artifact_data = match bounded_read(&artifact_path, 2 * 1024 * 1024) {
        Ok(d) => d,
        Err(e) => return err(&format!("artifact_read_failed: {e}")),
    };
    let manifest_raw = match bounded_read(&manifest_path, MAX_MANIFEST_SIZE) {
        Ok(d) => d,
        Err(e) => return err(&format!("manifest_read_failed: {e}")),
    };
    let evidence_data = match bounded_read(&evidence_path, MAX_EVIDENCE_SIZE) {
        Ok(d) => d,
        Err(e) => return err(&format!("evidence_read_failed: {e}")),
    };

    // ── Compute artifact digest via real Sha256Digest ──
    let artifact_digest = Sha256Digest::compute(&artifact_data);

    // ── Parse manifest as (possibly partial) HarnessManifest ──
    let manifest_value: Value = match serde_json::from_slice(&manifest_raw) {
        Ok(v) => v,
        Err(e) => return err(&format!("manifest_parse_failed: {e}")),
    };

    // Build a complete HarnessManifest from the workspace manifest JSON.
    // Fill in artifact_digest and compute manifest_id.
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: manifest_value
            .get("harness_id")
            .and_then(Value::as_str)
            .unwrap_or("coding_harness")
            .to_string(),
        artifact_digest: artifact_digest.as_str().to_string(),
        protocol_version: manifest_value
            .get("protocol_version")
            .and_then(Value::as_str)
            .unwrap_or("external-harness-v1")
            .to_string(),
        endpoint: manifest_value
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        operation_name: manifest_value
            .get("operation_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: manifest_value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        input_schema: manifest_value.get("input_schema").cloned().unwrap_or(
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        ),
        output_schema: manifest_value.get("output_schema").cloned().unwrap_or(
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        ),
        idempotent: manifest_value
            .get("idempotent")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        created_at: Utc::now(),
    };

    // Compute manifest ID using the real method.
    let manifest_id = match manifest.compute_manifest_id() {
        Ok(id) => id,
        Err(e) => return err(&format!("manifest_id_compute_failed: {e}")),
    };
    manifest.manifest_id = manifest_id;

    // ── Serialize final manifest ──
    let final_manifest_bytes = match serde_json::to_vec(&manifest) {
        Ok(b) => b,
        Err(e) => return err(&format!("manifest_serialize_failed: {e}")),
    };

    // ── Compute manifest digest and evidence digest ──
    let _manifest_digest = Sha256Digest::compute(&final_manifest_bytes);
    let _evidence_digest = Sha256Digest::compute(&evidence_data);

    // ── Store all three blobs in ContentStore ──
    let (stored_artifact_digest, stored_manifest_digest, stored_evidence_digest) = match (
        store.store(&artifact_data),
        store.store(&final_manifest_bytes),
        store.store(&evidence_data),
    ) {
        (Ok(a), Ok(m), Ok(e)) => (a, m, e),
        (Err(e), _, _) => return err(&format!("store_artifact_failed: {e}")),
        (_, Err(e), _) => return err(&format!("store_manifest_failed: {e}")),
        (_, _, Err(e)) => return err(&format!("store_evidence_failed: {e}")),
    };

    // ── Call existing Capability Proposal API ──
    let target_agent = manifest_value
        .get("target_agent_id")
        .and_then(Value::as_str)
        .unwrap_or("main");
    let risk = manifest_value
        .get("risk_summary")
        .and_then(Value::as_str)
        .unwrap_or("read-only coding harness proposal");

    let submit_body = json!({
        "target_agent_id": target_agent,
        "artifact_ref": artifact_rel,
        "artifact_digest": stored_artifact_digest.as_str(),
        "manifest_ref": manifest_rel,
        "manifest_digest": stored_manifest_digest.as_str(),
        "evidence_ref": evidence_rel,
        "evidence_digest": stored_evidence_digest.as_str(),
        "requested_operations": [manifest.operation_name.clone()],
        "risk_summary": risk,
    });

    match handle_submit_proposal(
        journal,
        gateway,
        &submit_body,
        "coding_harness",
        config_agent_id,
    ) {
        Ok(response) => ok(json!({
            "proposal_id": response.proposal_id,
            "status": response.status,
            "expected_active_snapshot_id": response.expected_active_snapshot_id,
            "requested_operations": response.requested_operations,
            "expires_at": response.expires_at,
            "artifact_digest": stored_artifact_digest.as_str(),
            "manifest_digest": stored_manifest_digest.as_str(),
            "evidence_digest": stored_evidence_digest.as_str(),
            "manifest_id": manifest.manifest_id,
            "operation_name": manifest.operation_name,
            "artifact_path": artifact_rel,
            "manifest_path": manifest_rel,
            "evidence_path": evidence_rel,
        })),
        Err(e) => err(&format!("proposal_submit_failed: {e}")),
    }
}

/// Read a file with a size limit.
fn bounded_read(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{e}"))?;
    let meta = f.metadata().map_err(|e| format!("{e}"))?;
    if meta.len() > max as u64 {
        return Err(format!("file_too_large: {} bytes", meta.len()));
    }
    let mut data = Vec::with_capacity((meta.len() as usize).min(max));
    f.read_to_end(&mut data).map_err(|e| format!("{e}"))?;
    Ok(data)
}
