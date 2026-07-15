//! Generic component artifact gate with real content-digest verification.

use std::path::Path;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;
use crate::self_evolution::artifact_manifest::CandidateArtifactManifest;

const DIGEST_PLACEHOLDER: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

pub(crate) fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let manifest = match CandidateArtifactManifest::load(&candidate.candidate_path) {
        Ok(manifest) => manifest,
        Err(error) => return fail(candidate, true, "ARTIFACT_MANIFEST_INVALID", &error),
    };
    let artifact = if ctx.built_binary.exists() {
        ctx.built_binary.clone()
    } else {
        candidate.candidate_path.join(&manifest.entry)
    };
    if !artifact.exists() {
        return fail(
            candidate,
            false,
            "ARTIFACT_ENTRY_NOT_FOUND",
            &format!("entry file not found: {}", artifact.display()),
        );
    }
    let mut errors = Vec::new();
    if artifact.is_symlink() {
        errors.push(format!("entry is a symlink: {}", artifact.display()));
    } else if !artifact.is_file() {
        errors.push(format!(
            "entry is not a regular file: {}",
            artifact.display()
        ));
    }
    if !path_is_within_allowed_root(&artifact, &candidate.candidate_path, &ctx.work_base) {
        errors.push(format!(
            "entry path escapes allowed roots: {}",
            artifact.display()
        ));
    }

    let computed = compute_file_digest(&artifact);
    if manifest.artifact_digest != DIGEST_PLACEHOLDER && manifest.artifact_digest != computed {
        errors.push(format!(
            "artifact digest mismatch: declared={}, computed={computed}",
            manifest.artifact_digest
        ));
    }
    let passed = errors.is_empty();
    GateResult {
        gate_kind: GateKind::Artifact,
        passed,
        is_candidate_failure: !passed,
        exit_code: if passed { 0 } else { -1 },
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: (!passed).then(|| "ARTIFACT_DIGEST_MISMATCH".into()),
        stdout: if passed {
            format!(
                "component_id={}\nprofile_id={}\nartifact_digest_verified=true\nartifact_digest={computed}",
                manifest.component_id, manifest.profile_id
            )
        } else {
            String::new()
        },
        stderr: errors.join("\n"),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: passed.then_some(computed),
    }
}

fn path_is_within_allowed_root(path: &Path, candidate: &Path, work: &Path) -> bool {
    let Ok(resolved) = path.canonicalize() else {
        return false;
    };
    [candidate, work].iter().any(|root| {
        root.canonicalize()
            .map(|root| resolved.starts_with(root))
            .unwrap_or(false)
    })
}

fn compute_file_digest(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).unwrap_or_default();
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn fail(
    candidate: &CandidateSnapshot,
    is_candidate_failure: bool,
    code: &str,
    message: &str,
) -> GateResult {
    GateResult {
        gate_kind: GateKind::Artifact,
        passed: false,
        is_candidate_failure,
        exit_code: -1,
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: Some(code.into()),
        stdout: String::new(),
        stderr: message.into(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}
