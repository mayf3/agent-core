//! Generic component scaffold gate.

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;
use crate::self_evolution::artifact_manifest::CandidateArtifactManifest;

pub(crate) fn check(candidate: &CandidateSnapshot, _ctx: &GateContext) -> GateResult {
    let candidate_path = &candidate.candidate_path;
    let mut errors = Vec::new();

    let cargo_toml = candidate_path.join("Cargo.toml");
    match std::fs::read_to_string(&cargo_toml) {
        Ok(value) if value.contains("[package]") => {}
        Ok(_) => errors.push("Cargo.toml missing [package] section".into()),
        Err(error) => errors.push(format!("Cargo.toml read error: {error}")),
    }
    if !candidate_path.join("src/main.rs").is_file() {
        errors.push("src/main.rs not found".into());
    }
    let manifest = match CandidateArtifactManifest::load(candidate_path) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            errors.push(error);
            None
        }
    };
    if manifest
        .as_ref()
        .is_some_and(|value| value.test_kit == "hook-consumer-service-contract-v0")
    {
        if !candidate_path.join("Cargo.lock").is_file() {
            errors.push("generated hook consumer requires Cargo.lock".into());
        }
        if let Ok(cargo) = std::fs::read_to_string(&cargo_toml) {
            for forbidden in ["path =", "git =", "build =", "workspace ="] {
                if cargo.contains(forbidden) {
                    errors.push(format!(
                        "generated hook consumer Cargo.toml contains forbidden source: {forbidden}"
                    ));
                }
            }
        }
    }

    let passed = errors.is_empty();
    GateResult {
        gate_kind: GateKind::Scaffold,
        passed,
        is_candidate_failure: !passed,
        exit_code: if passed { 0 } else { -1 },
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: (!passed).then(|| "SCAFFOLD_FAILED".into()),
        stdout: manifest
            .map(|manifest| {
                format!(
                    "component_id={}\nprofile_id={}\ntest_kit={}",
                    manifest.component_id, manifest.profile_id, manifest.test_kit
                )
            })
            .unwrap_or_default(),
        stderr: errors.join("\n"),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}
