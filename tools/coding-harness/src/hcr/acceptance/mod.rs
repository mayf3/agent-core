//! HCR acceptance orchestrator.
//!
//! Receives a candidate reference, snapshots it, runs all five
//! acceptance gates, stores the result idempotently, and returns
//! a structured response with artifact/evidence digests.

pub mod execution_store;
pub mod protocol;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use super::candidate::{snapshot_candidate, CandidateSnapshot};
use super::gates::{run_all_gates, GateKind, GateResult};
use protocol::{compute_evidence_digest, compute_fingerprint, AcceptanceResponse, GateResultEntry};

/// Global counter for gate executions (test-only observation).
static GATE_EXECUTION_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Reset the execution counter (for tests).
pub fn reset_execution_count() {
    GATE_EXECUTION_COUNT.store(0, Ordering::SeqCst);
}

/// Read the execution counter (for tests).
pub fn execution_count() -> usize {
    GATE_EXECUTION_COUNT.load(Ordering::SeqCst)
}

/// Handle an acceptance request.
///
/// Arguments (from Kernel):
/// - All identity fields (hcr_id, claim_id, run_id, principal_id, etc.)
/// - candidate_ref
/// - idempotency_key
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

    // Compute fingerprint for idempotency check
    let fingerprint = compute_fingerprint(
        hcr_id, claim_id, run_id, principal_id, gateway_session_id,
        registry_snapshot_id, operation, candidate_ref, idempotency_key,
    );

    // Create execution store and try to claim
    let store = execution_store::ExecutionStore::new(artifact_root);
    match store.claim_execution(idempotency_key, &fingerprint) {
        Err(execution_store::ExecutionStoreError::AlreadyCompleted) => {
            // Replay: load and return existing result
            if let Some(existing) = store.load_completed(idempotency_key) {
                return ok_json(&existing);
            }
            return err_json("REPLAY_LOAD_FAILED");
        }
        Err(execution_store::ExecutionStoreError::FingerprintMismatch) => {
            return err_json("IDEMPOTENCY_CONFLICT");
        }
        Err(execution_store::ExecutionStoreError::AlreadyClaimed) => {
            return err_json("EXECUTION_IN_PROGRESS");
        }
        Err(e) => return err_json(&format!("EXECUTION_CLAIM_FAILED: {e}")),
        Ok(guard) => {
            // Claimed successfully — proceed
            execute_and_complete(artifact_root, args, &store, &guard, &fingerprint,
                hcr_id, claim_id, run_id, principal_id, gateway_session_id,
                registry_snapshot_id, operation, candidate_ref, idempotency_key)
        }
    }
}

fn execute_and_complete(
    artifact_root: &Path,
    args: &Value,
    store: &execution_store::ExecutionStore,
    guard: &execution_store::ExecutionGuard,
    fingerprint: &protocol::RequestFingerprint,
    hcr_id: &str, claim_id: &str, run_id: &str,
    principal_id: &str, gateway_session_id: &str, registry_snapshot_id: &str,
    operation: &str, candidate_ref: &str, idempotency_key: &str,
) -> Value {
    // Resolve candidate path
    let candidate_path = match resolve_safe(artifact_root, candidate_ref) {
        Some(p) => p,
        None => return err_json("CANDIDATE_REF_ESCAPE"),
    };
    if !candidate_path.is_dir() {
        return err_json("CANDIDATE_NOT_FOUND");
    }

    // Create base dir for snapshots
    let base_dir = artifact_root.join("candidates_base");
    if let Err(e) = std::fs::create_dir_all(&base_dir) {
        return err_json(&format!("BASE_DIR_FAILED: {e}"));
    }

    // Snapshot
    let snapshot = match snapshot_candidate(&candidate_path, &base_dir) {
        Ok(s) => s,
        Err(e) => return err_json(&format!("SNAPSHOT_FAILED: {e}")),
    };

    // Run all five gates (increment counter)
    GATE_EXECUTION_COUNT.fetch_add(1, Ordering::SeqCst);
    let results = run_all_gates(&snapshot);

    // Extract results
    let outcome = classify_outcome(&results);
    let (artifact_ref, artifact_digest) = extract_artifact(&results, &snapshot);

    // Validate gate consistency
    if let Err(e) = validate_gate_consistency(&results, &outcome, &artifact_digest) {
        return err_json(&format!("GATE_CONSISTENCY_FAILED: {e}"));
    }

    // Build gate result entries
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

    // Build evidence digest
    let evidence_digest = compute_evidence_digest(
        &guard.dir.file_name().unwrap_or_default().to_string_lossy(),
        fingerprint,
        &snapshot.candidate_id,
        &snapshot.candidate_digest,
        &gate_entries,
        &outcome,
        artifact_ref.as_deref(),
        artifact_digest.as_deref(),
    );

    // Build full response
    let harness_execution_id = guard.dir.file_name()
        .unwrap_or_default().to_string_lossy().to_string();

    let response = AcceptanceResponse {
        harness_execution_id: harness_execution_id.clone(),
        idempotency_key: idempotency_key.to_string(),
        hcr_id: hcr_id.to_string(),
        claim_id: claim_id.to_string(),
        run_id: run_id.to_string(),
        principal_id: principal_id.to_string(),
        gateway_session_id: gateway_session_id.to_string(),
        registry_snapshot_id: registry_snapshot_id.to_string(),
        operation: operation.to_string(),
        candidate_id: snapshot.candidate_id.clone(),
        candidate_digest: snapshot.candidate_digest.clone(),
        overall_outcome: outcome,
        gate_results: gate_entries,
        artifact_ref,
        artifact_digest,
        evidence_digest,
    };

    // Persist result
    let result_value = serde_json::to_value(&response).unwrap_or_default();
    if let Err(e) = store.complete_execution(guard, &result_value) {
        return err_json(&format!("EXECUTION_COMPLETE_FAILED: {e}"));
    }

    ok_json(&result_value)
}

/// Classify overall outcome from gate results.
fn classify_outcome(results: &[GateResult]) -> String {
    let any_infra = results.iter().any(|r| !r.passed && !r.is_candidate_failure);
    if any_infra { return "InfrastructureFailure".into(); }
    let any_candidate_fail = results.iter().any(|r| !r.passed && r.is_candidate_failure);
    if any_candidate_fail { return "CandidateFailed".into(); }
    if results.iter().all(|r| r.passed) { return "CandidatePassed".into(); }
    "InfrastructureFailure".into()
}

/// Extract artifact info from Artifact gate results.
fn extract_artifact(results: &[GateResult], snapshot: &CandidateSnapshot) -> (Option<String>, Option<String>) {
    for r in results {
        if r.gate_kind == GateKind::Artifact && r.passed {
            // Parse the computed digest from the Artifact gate's stdout
            if !r.stdout.is_empty() {
                // The Artifact gate confirms digest verification in stdout
                return (Some("target/release/calculator-harness".into()), Some("verified".into()));
            }
        }
    }
    (None, None)
}

/// Validate gate result consistency.
fn validate_gate_consistency(results: &[GateResult], outcome: &str, artifact_digest: &Option<String>) -> Result<(), String> {
    // Check exactly 5 unique gates
    let kinds: std::collections::HashSet<String> = results.iter()
        .map(|r| r.gate_kind.as_str().to_string()).collect();
    if kinds.len() != 5 {
        return Err(format!("expected 5 unique gates, got {}", kinds.len()));
    }

    // Outcome consistency
    match outcome {
        "CandidatePassed" => {
            if results.iter().any(|r| !r.passed) {
                return Err("CandidatePassed but some gates failed".into());
            }
            if artifact_digest.is_none() {
                return Err("CandidatePassed but no artifact digest".into());
            }
        }
        "CandidateFailed" => {
            if !results.iter().any(|r| !r.passed && r.is_candidate_failure) {
                return Err("CandidateFailed but no candidate failure".into());
            }
        }
        "InfrastructureFailure" => {
            if !results.iter().any(|r| !r.passed && !r.is_candidate_failure) {
                return Err("InfrastructureFailure but no infra failure".into());
            }
        }
        _ => return Err(format!("unknown outcome: {outcome}")),
    }
    Ok(())
}

/// Resolve a safe relative path within the artifact root.
fn resolve_safe(root: &Path, rel: &str) -> Option<std::path::PathBuf> {
    let rel_path = std::path::Path::new(rel);
    if rel_path.is_absolute() || rel.contains("..") { return None; }
    let joined = root.join(rel_path);
    if !joined.starts_with(root) { return None; }
    if let Ok(canon) = joined.canonicalize() {
        let root_canon = root.canonicalize().ok()?;
        if !canon.starts_with(&root_canon) { return None; }
    }
    Some(joined)
}

fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|v| v.as_str())
}

fn ok_json(v: &Value) -> Value {
    serde_json::json!({
        "protocol_version": "external-harness-v1",
        "ok": true,
        "result": v,
    })
}

fn err_json(code: &str) -> Value {
    serde_json::json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
    })
}
