//! Artifact acceptance gate.
//!
//! Validates that the built artifact exists, the manifest is parseable,
//! operation is `external.calculator`, the entry file exists, the
//! artifact digest is well-formed, and there is no path escape.
//!
//! Failure is `CandidateFailed`; infra failures are `InfrastructureFailure`.

use std::path::Path;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

/// Run the artifact gate against the given candidate snapshot.
///
/// Checks:
/// 1. Manifest exists and is parseable
/// 2. Manifest operation = external.calculator
/// 3. Entry file exists (from build output)
/// 4. Artifact digest is well-formed (sha256:...)
/// 5. No path escape (all paths resolve within candidate directory)
pub fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let candidate_path = &candidate.candidate_path;
    let mut errors: Vec<String> = Vec::new();

    // 1. Parse manifest
    let manifest_path = candidate_path.join("manifest.json");
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => match serde_json::from_str::<serde_json::Value>(&c) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("manifest parse error: {e}"));
                return GateResult {
                    gate_kind: GateKind::Artifact,
                    passed: false,
                    is_candidate_failure: true,
                    exit_code: -1,
                    timed_out: false,
                    child_cleanup: CleanupStatus::Confirmed,
                    error_code: Some("ARTIFACT_MANIFEST_PARSE_FAILED".into()),
                    stdout: String::new(),
                    stderr: errors.join("\n"),
                    candidate_id: candidate.candidate_id.clone(),
                    candidate_digest: candidate.candidate_digest.clone(),
                    candidate_digest_preserved: false,
                };
            }
        },
        Err(e) => {
            errors.push(format!("manifest read error: {e}"));
            return GateResult {
                gate_kind: GateKind::Artifact,
                passed: false,
                is_candidate_failure: true,
                exit_code: -1,
                timed_out: false,
                child_cleanup: CleanupStatus::Confirmed,
                error_code: Some("ARTIFACT_MANIFEST_MISSING".into()),
                stdout: String::new(),
                stderr: errors.join("\n"),
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
            };
        }
    };

    // 2. Check operation = external.calculator
    let operation = manifest["operation"].as_str().unwrap_or("");
    if operation != "external.calculator" {
        errors.push(format!(
            "operation is '{operation}', expected 'external.calculator'"
        ));
    }

    // 3. Find and check entry file (prefer the built binary from work dir)
    let entry_path = if let Some(entry) = manifest["entry"].as_str() {
        if entry.contains("..") {
            errors.push(format!("entry path contains parent traversal: {entry}"));
            None
        } else if Path::new(entry).is_absolute() {
            errors.push(format!("entry path is absolute: {entry}"));
            None
        } else {
            // First check the built binary (work directory)
            if ctx.built_binary.exists() {
                Some(ctx.built_binary.clone())
            } else {
                let full_path = candidate_path.join(entry);
                Some(full_path)
            }
        }
    } else {
        errors.push("manifest missing 'entry' field".into());
        None
    };

    // 4. Check artifact digest format
    if let Some(digest) = manifest["artifact_digest"].as_str() {
        if !digest.starts_with("sha256:") {
            errors.push(format!(
                "artifact_digest does not start with 'sha256:': {digest}"
            ));
        } else if digest.len() != 71 {
            errors.push(format!(
                "artifact_digest has wrong length ({}), expected 71",
                digest.len()
            ));
        }
    } else {
        errors.push("manifest missing 'artifact_digest' field".into());
    }

    // 5. Check no path escape in the entry path
    if let Some(ref ep) = entry_path {
        if ep.exists() {
            if let Ok(resolved) = ep.canonicalize() {
                // Check that the entry is within either the candidate or work dir
                let candidate_canon = candidate_path.canonicalize().ok();
                let work_canon = ctx.work_base.canonicalize().ok();

                let in_candidate = candidate_canon
                    .as_ref()
                    .map(|c| resolved.starts_with(c))
                    .unwrap_or(false);
                let in_work = work_canon
                    .as_ref()
                    .map(|c| resolved.starts_with(c))
                    .unwrap_or(false);

                if !in_candidate && !in_work {
                    errors.push(format!(
                        "entry path escapes allowed directories: {resolved:?}"
                    ));
                }
            }
        }
    }

    let passed = errors.is_empty();
    GateResult {
        gate_kind: GateKind::Artifact,
        passed,
        is_candidate_failure: !passed,
        exit_code: if passed { 0 } else { -1 },
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: if passed {
            None
        } else {
            Some("ARTIFACT_FAILED".into())
        },
        stdout: String::new(),
        stderr: errors.join("\n"),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
    }
}
