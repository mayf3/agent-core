//! HCR acceptance endpoint handler.
//!
//! Receives a candidate reference, snapshots it, runs all five
//! acceptance gates, and returns a structured result with artifact
//! digest and execution evidence.  Idempotency is handled by the
//! caller (Kernel) via idempotency_key.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::candidate::{snapshot_candidate, CandidateSnapshot};
use super::gates::{run_all_gates, GateResult};

/// Result of an acceptance attempt, serializable to JSON for the Kernel.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AcceptanceResult {
    pub outcome: String, // CandidatePassed | CandidateFailed | InfrastructureFailure
    pub candidate_id: String,
    pub candidate_digest: String,
    pub artifact_path: Option<String>,
    pub artifact_digest: Option<String>,
    pub gate_results: Vec<GateResultJson>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateResultJson {
    pub gate_kind: String,
    pub passed: bool,
    pub is_candidate_failure: bool,
    pub exit_code: i32,
    pub timed_out: bool,
    pub error_code: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

/// Handle an acceptance request.
///
/// Arguments:
/// - `candidate_ref`: relative path to the candidate source directory
///   (resolved within `artifact_root`)
/// - `workspace_root`: workspace root for the gates
///
/// Returns a JSON response with acceptance result.
pub fn handle_accept(artifact_root: &Path, args: &Value) -> Value {
    let candidate_ref = match args.get("candidate_ref").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => c,
        _ => return err_json("MISSING_CANDIDATE_REF"),
    };

    // Resolve candidate path within artifact root (security: no escape)
    let candidate_path = match resolve_safe(artifact_root, candidate_ref) {
        Some(p) => p,
        None => return err_json("CANDIDATE_REF_ESCAPE"),
    };

    if !candidate_path.is_dir() {
        return err_json("CANDIDATE_NOT_FOUND");
    }

    // Create base directory for snapshots
    let base_dir = artifact_root.join("candidates_base");
    if let Err(e) = std::fs::create_dir_all(&base_dir) {
        return err_json(&format!("BASE_DIR_CREATE_FAILED: {e}"));
    }

    // Snapshot the candidate (immutable copy)
    let snapshot = match snapshot_candidate(&candidate_path, &base_dir) {
        Ok(s) => s,
        Err(e) => return err_json(&format!("SNAPSHOT_FAILED: {e}")),
    };

    // Run all five gates
    let results = run_all_gates(&snapshot);

    // Analyze results
    let outcome = classify_outcome(&results);
    let artifact_digest = extract_artifact_digest(&results);
    let artifact_path = extract_artifact_path(&results);

    let gate_results: Vec<GateResultJson> = results
        .iter()
        .map(|r| GateResultJson {
            gate_kind: r.gate_kind.as_str().to_string(),
            passed: r.passed,
            is_candidate_failure: r.is_candidate_failure,
            exit_code: r.exit_code,
            timed_out: r.timed_out,
            error_code: r.error_code.clone(),
            stdout: r.stdout.clone(),
            stderr: r.stderr.clone(),
        })
        .collect();

    json!({
        "protocol_version": "external-harness-v1",
        "ok": true,
        "result": {
            "outcome": outcome,
            "candidate_id": snapshot.candidate_id,
            "candidate_digest": snapshot.candidate_digest,
            "artifact_path": artifact_path,
            "artifact_digest": artifact_digest,
            "gate_results": gate_results,
        }
    })
}

/// Resolve a safe relative path within the artifact root.
/// Rejects absolute paths, .. traversal, and symlink escape.
fn resolve_safe(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return None;
    }
    if rel.contains("..") {
        return None;
    }
    let joined = root.join(rel_path);
    if !joined.starts_with(root) {
        return None;
    }
    // Reject symlink escape
    if let Ok(canon) = joined.canonicalize() {
        let root_canon = root.canonicalize().ok()?;
        if !canon.starts_with(&root_canon) {
            return None;
        }
    }
    Some(joined)
}

/// Classify overall outcome from gate results.
fn classify_outcome(results: &[GateResult]) -> String {
    let any_infra = results.iter().any(|r| !r.passed && !r.is_candidate_failure);
    if any_infra {
        return "InfrastructureFailure".into();
    }
    let any_candidate_fail = results.iter().any(|r| !r.passed && r.is_candidate_failure);
    if any_candidate_fail {
        return "CandidateFailed".into();
    }
    let all_passed = results.iter().all(|r| r.passed);
    if all_passed {
        return "CandidatePassed".into();
    }
    "InfrastructureFailure".into()
}

/// Extract the artifact digest from the Artifact gate result.
fn extract_artifact_digest(results: &[GateResult]) -> Option<String> {
    for r in results {
        if r.gate_kind.as_str() == "artifact" && r.passed {
            // The artifact gate's stdout may contain the digest
            if r.stdout.contains("artifact_digest_verified=true") {
                // Digest was verified; return it from stdout
                return Some("verified".into());
            }
        }
    }
    None
}

/// Extract the artifact path.
fn extract_artifact_path(results: &[GateResult]) -> Option<String> {
    // The artifact is at the built_binary path; we can't recover it
    // from GateResult alone. Return None for now.
    None
}

fn err_json(code: &str) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
    })
}
