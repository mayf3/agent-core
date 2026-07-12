//! Artifact acceptance gate.
//!
//! Validates that the built artifact exists, the manifest is parseable,
//! operation is `external.calculator`, and — critically — that the
//! **real artifact content digest** matches the manifest's declared
//! digest (B1). Format-only digest checks are insufficient.
//!
//! Also checks: no path escape, no symlink artifact, entry file exists.
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
/// 3. Entry file exists (from build output), not a symlink
/// 4. **Real artifact content SHA-256 matches manifest declaration** (B1)
/// 5. No path escape
pub fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let candidate_path = &candidate.candidate_path;
    let mut errors: Vec<String> = Vec::new();

    // 1. Parse manifest
    let manifest_path = candidate_path.join("manifest.json");
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => match serde_json::from_str::<serde_json::Value>(&c) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    GateKind::Artifact,
                    true,
                    "ARTIFACT_MANIFEST_PARSE_FAILED",
                    &format!("manifest parse error: {e}"),
                    candidate,
                );
            }
        },
        Err(e) => {
            return fail(
                GateKind::Artifact,
                true,
                "ARTIFACT_MANIFEST_MISSING",
                &format!("manifest read error: {e}"),
                candidate,
            );
        }
    };

    // 2. Check operation
    let operation = manifest["operation"].as_str().unwrap_or("");
    if operation != "external.calculator" {
        errors.push(format!(
            "operation is '{operation}', expected 'external.calculator'"
        ));
    }

    // 3. Find entry file — prefer the built binary from work dir
    let declared_digest = manifest["artifact_digest"].as_str().map(|s| s.to_string());

    let entry_path = if let Some(entry) = manifest["entry"].as_str() {
        if entry.contains("..") {
            errors.push(format!("entry path contains parent traversal: {entry}"));
            None
        } else if Path::new(entry).is_absolute() {
            errors.push(format!("entry path is absolute: {entry}"));
            None
        } else {
            let fp = if ctx.built_binary.exists() {
                ctx.built_binary.clone()
            } else {
                candidate_path.join(entry)
            };
            Some(fp)
        }
    } else {
        errors.push("manifest missing 'entry' field".into());
        None
    };

    // 4. Verify real artifact digest (B1)
    let artifact_digest_verified = match (&entry_path, &declared_digest) {
        (Some(ep), Some(declared)) => {
            if !ep.exists() {
                // Entry not found — likely Build gate didn't produce it.
                // This is infrastructure failure, not candidate failure.
                return GateResult {
                    gate_kind: GateKind::Artifact,
                    passed: false,
                    is_candidate_failure: false,
                    exit_code: -1,
                    timed_out: false,
                    child_cleanup: CleanupStatus::Confirmed,
                    error_code: Some("ARTIFACT_ENTRY_NOT_FOUND".into()),
                    stdout: String::new(),
                    stderr: format!("entry file not found: {}", ep.display()),
                    candidate_id: candidate.candidate_id.clone(),
                    candidate_digest: candidate.candidate_digest.clone(),
                    candidate_digest_preserved: false,
                };
            } else if ep.is_symlink() {
                errors.push(format!("entry is a symlink, rejecting: {}", ep.display()));
                false
            } else if !ep.is_file() {
                errors.push(format!("entry is not a regular file: {}", ep.display()));
                false
            } else {
                // Compute real content digest
                let computed = compute_file_digest(ep);
                if !declared.starts_with("sha256:") || declared.len() != 71 {
                    errors.push(format!("declared digest has invalid format: {declared}"));
                    false
                } else if computed != *declared {
                    errors.push(format!(
                        "artifact digest mismatch: declared={declared}, computed={computed}"
                    ));
                    false
                } else {
                    // Digest matches — real content verified
                    true
                }
            }
        }
        (None, _) => false,
        (_, None) => {
            errors.push("manifest missing 'artifact_digest' field".into());
            false
        }
    };

    // 5. Check no path escape
    if let Some(ref ep) = entry_path {
        if ep.exists() {
            // Reject symlinks at the entry path (already checked above, but
            // also check for symlink components in the resolved path)
            if let Ok(resolved) = ep.canonicalize() {
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
                    errors.push(format!("entry path escapes allowed dirs: {resolved:?}"));
                }
            }
        }
    }

    let passed = errors.is_empty() && artifact_digest_verified;
    let error_code = if !passed {
        if !artifact_digest_verified {
            Some("ARTIFACT_DIGEST_MISMATCH".into())
        } else {
            Some("ARTIFACT_FAILED".into())
        }
    } else {
        None
    };

    GateResult {
        gate_kind: GateKind::Artifact,
        passed,
        is_candidate_failure: !passed,
        exit_code: if passed { 0 } else { -1 },
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code,
        stdout: if passed {
            format!("artifact_digest_verified=true")
        } else {
            String::new()
        },
        stderr: errors.join("\n"),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
    }
}

/// Compute the SHA-256 digest of a file.
fn compute_file_digest(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(path).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hex = hex::encode(hasher.finalize());
    format!("sha256:{hex}")
}

/// Helper to create a quick failure result.
fn fail(
    kind: GateKind,
    is_candidate_failure: bool,
    error_code: &str,
    message: &str,
    candidate: &CandidateSnapshot,
) -> GateResult {
    GateResult {
        gate_kind: kind,
        passed: false,
        is_candidate_failure,
        exit_code: -1,
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: Some(error_code.to_string()),
        stdout: String::new(),
        stderr: message.to_string(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
    }
}
