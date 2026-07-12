//! HCR acceptance orchestrator.
//!
//! Receives a candidate reference, snapshots it, runs all five
//! acceptance gates under OS file lock (H7), persists the result
//! atomically, and returns a structured response with artifact
//! and evidence digests (H4/H5).

pub mod execution_store;
pub mod protocol;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use super::candidate::{snapshot_candidate, CandidateSnapshot};
use super::gates::{run_all_gates, GateKind, GateResult};
use execution_store::ExecutionStore;
use protocol::{
    compute_evidence_digest, compute_fingerprint, AcceptanceResponse, GateResultEntry,
};

/// Global gate execution counter (test observation only).
static GATE_EXECUTION_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn reset_execution_count() { GATE_EXECUTION_COUNT.store(0, Ordering::SeqCst); }
pub fn execution_count() -> usize { GATE_EXECUTION_COUNT.load(Ordering::SeqCst) }

/// Handle an acceptance request. Dispatches through ExecutionStore for
/// idempotency, locking, crash recovery (H7), and atomic persistence.
pub fn handle_accept(artifact_root: &Path, args: &Value) -> Value {
    let idempotency_key = get_str(args, "idempotency_key").unwrap_or("");
    let hcr_id = get_str(args, "hcr_id").unwrap_or("");
    let claim_id = get_str(args, "claim_id").unwrap_or("");
    let run_id = get_str(args, "run_id").unwrap_or("");
    let principal_id = get_str(args, "principal_id").unwrap_or("");
    let gateway_session_id = get_str(args, "gateway_session_id").unwrap_or("");
    let registry_snapshot_id = get_str(args, "registry_snapshot_id").unwrap_or("");
    let operation = get_str(args, "operation").unwrap_or("external.coding_hcr_accept");
    let candidate_ref = match get_str(args, "candidate_ref") {
        Some(c) if !c.is_empty() => c,
        _ => return err_json("MISSING_CANDIDATE_REF"),
    };

    if idempotency_key.is_empty() {
        return err_json("MISSING_IDEMPOTENCY_KEY");
    }

    let fingerprint = compute_fingerprint(
        hcr_id, claim_id, run_id, principal_id, gateway_session_id,
        registry_snapshot_id, operation, candidate_ref, idempotency_key,
    );

    let store = ExecutionStore::new(artifact_root);

    // Execute under OS file lock (H7): crash-safe, idempotent
    match store.execute(idempotency_key, &fingerprint, || {
        GATE_EXECUTION_COUNT.fetch_add(1, Ordering::SeqCst);
        do_accept(artifact_root, args, &fingerprint,
                  hcr_id, claim_id, run_id, principal_id, gateway_session_id,
                  registry_snapshot_id, operation, candidate_ref, idempotency_key)
            .and_then(|resp| serde_json::to_value(resp).map_err(|e| e.to_string()))
    }) {
        Ok(result) => ok_json(&result),
        Err(execution_store::ExecutionStoreError::FingerprintMismatch(_)) => {
            err_json("IDEMPOTENCY_CONFLICT")
        }
        Err(execution_store::ExecutionStoreError::LockFailed(e)) => {
            err_json(&format!("LOCK_FAILED: {e}"))
        }
        Err(e) => err_json(&format!("EXECUTION_FAILED: {e}")),
    }
}

/// Core acceptance logic (runs under file lock, called at most once per key).
fn do_accept(
    artifact_root: &Path, _args: &Value,
    fingerprint: &protocol::RequestFingerprint,
    hcr_id: &str, claim_id: &str, run_id: &str,
    principal_id: &str, gateway_session_id: &str, registry_snapshot_id: &str,
    operation: &str, candidate_ref: &str, idempotency_key: &str,
) -> Result<AcceptanceResponse, String> {
    let candidate_path = resolve_safe(artifact_root, candidate_ref)
        .ok_or_else(|| "CANDIDATE_REF_ESCAPE".to_string())?;
    if !candidate_path.is_dir() {
        return Err("CANDIDATE_NOT_FOUND".to_string());
    }

    let base_dir = artifact_root.join("candidates_base");
    std::fs::create_dir_all(&base_dir).map_err(|e| format!("BASE_DIR: {e}"))?;

    let snapshot = snapshot_candidate(&candidate_path, &base_dir)
        .map_err(|e| format!("SNAPSHOT: {e}"))?;

    let results = run_all_gates(&snapshot);
    let outcome = classify_outcome(&results);
    let (artifact_ref, artifact_digest) = extract_artifact(&results);

    validate_gate_consistency(&results, &outcome, &artifact_digest)?;

    let gate_entries: Vec<GateResultEntry> = results.iter().map(|r| GateResultEntry {
        gate_kind: r.gate_kind.as_str().to_string(),
        passed: r.passed,
        is_candidate_failure: r.is_candidate_failure,
        exit_code: r.exit_code,
        timed_out: r.timed_out,
        error_code: r.error_code.clone(),
        stdout: r.stdout.clone(),
        stderr: r.stderr.clone(),
    }).collect();

    let harness_execution_id = sha256_prefix(idempotency_key);

    let evidence_digest = compute_evidence_digest(
        &harness_execution_id, fingerprint,
        &snapshot.candidate_id, &snapshot.candidate_digest,
        &gate_entries, &outcome,
        artifact_ref.as_deref(), artifact_digest.as_deref(),
    );

    Ok(AcceptanceResponse {
        harness_execution_id,
        idempotency_key: idempotency_key.to_string(),
        hcr_id: hcr_id.to_string(),
        claim_id: claim_id.to_string(),
        run_id: run_id.to_string(),
        principal_id: principal_id.to_string(),
        gateway_session_id: gateway_session_id.to_string(),
        registry_snapshot_id: registry_snapshot_id.to_string(),
        operation: operation.to_string(),
        candidate_id: snapshot.candidate_id,
        candidate_digest: snapshot.candidate_digest,
        overall_outcome: outcome,
        gate_results: gate_entries,
        artifact_ref,
        artifact_digest,
        evidence_digest,
    })
}

fn sha256_prefix(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))[..16].to_string()
}

fn classify_outcome(results: &[GateResult]) -> String {
    if results.iter().any(|r| !r.passed && !r.is_candidate_failure) { return "InfrastructureFailure".into(); }
    if results.iter().any(|r| !r.passed && r.is_candidate_failure) { return "CandidateFailed".into(); }
    if results.iter().all(|r| r.passed) { return "CandidatePassed".into(); }
    "InfrastructureFailure".into()
}

fn extract_artifact(results: &[GateResult]) -> (Option<String>, Option<String>) {
    for r in results {
        if r.gate_kind == GateKind::Artifact && r.passed {
            let digest = r.computed_artifact_digest.clone().unwrap_or_else(|| "unknown".into());
            return (Some("target/release/calculator-harness".into()), Some(digest));
        }
    }
    (None, None)
}

fn validate_gate_consistency(results: &[GateResult], outcome: &str, artifact_digest: &Option<String>) -> Result<(), String> {
    let kinds: std::collections::HashSet<String> = results.iter().map(|r| r.gate_kind.as_str().to_string()).collect();
    if kinds.len() != 5 { return Err(format!("expected 5 gates, got {}", kinds.len())); }
    match outcome {
        "CandidatePassed" => {
            if results.iter().any(|r| !r.passed) { return Err("CandidatePassed but gates failed".into()); }
            if artifact_digest.is_none() { return Err("CandidatePassed missing artifact_digest".into()); }
        }
        "CandidateFailed" => {
            if !results.iter().any(|r| !r.passed && r.is_candidate_failure) { return Err("CandidateFailed but no failure".into()); }
        }
        "InfrastructureFailure" => {
            if !results.iter().any(|r| !r.passed && !r.is_candidate_failure) { return Err("InfraFailure but no infra".into()); }
        }
        _ => return Err(format!("unknown outcome: {outcome}")),
    }
    Ok(())
}

fn resolve_safe(root: &Path, rel: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(rel);
    if p.is_absolute() || rel.contains("..") { return None; }
    let j = root.join(p);
    if !j.starts_with(root) { return None; }
    if let Ok(c) = j.canonicalize() {
        if let Ok(rc) = root.canonicalize() { if !c.starts_with(&rc) { return None; } }
    }
    Some(j)
}

fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|v| v.as_str())
}

fn ok_json(v: &Value) -> Value {
    serde_json::json!({"protocol_version":"external-harness-v1","ok":true,"result":v})
}
fn err_json(code: &str) -> Value {
    serde_json::json!({"protocol_version":"external-harness-v1","ok":false,"error_code":code})
}
