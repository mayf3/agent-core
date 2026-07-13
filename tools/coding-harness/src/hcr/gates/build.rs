//! Build acceptance gate.
//!
//! Copies the candidate source to a writable work directory, then
//! executes `cargo build --release` with mandatory sandboxed process
//! execution. Sandbox unavailable = InfrastructureFailure (B2).
//!
//! Build failure is `CandidateFailed`; spawn/setup failures are
//! `InfrastructureFailure`.

use std::path::Path;
use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

/// Run the build gate against the given candidate snapshot.
///
/// Copies the candidate source to `ctx.build_source` (writable), then
/// runs `cargo build --release` with `CARGO_TARGET_DIR=ctx.build_target`.
pub fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    // Step 1: Copy candidate source to writable build directory
    let target_dir = &ctx.build_target;
    let build_source = &ctx.build_source;

    if let Err(e) = std::fs::create_dir_all(target_dir) {
        return GateResult::infrastructure_failure(
            GateKind::Build,
            "BUILD_SETUP_FAILED",
            &format!("failed to create target directory: {e}"),
            candidate,
        );
    }
    if let Err(e) = std::fs::create_dir_all(build_source) {
        return GateResult::infrastructure_failure(
            GateKind::Build,
            "BUILD_SETUP_FAILED",
            &format!("failed to create build source directory: {e}"),
            candidate,
        );
    }

    if let Err(e) = copy_candidate_source(&candidate.candidate_path, build_source) {
        return GateResult::infrastructure_failure(
            GateKind::Build,
            "BUILD_SETUP_FAILED",
            &format!("failed to copy candidate source: {e}"),
            candidate,
        );
    }

    let manifest_path = build_source.join("Cargo.toml");

    // Pass through rust/cargo home directories so stable toolchain is found
    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{h}/.rustup"))
            .unwrap_or_default()
    });
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{h}/.cargo"))
            .unwrap_or_default()
    });

    // Run cargo build with mandatory sandbox (B2: fail-closed)
    let result = super::run_command_sandboxed(
        std::path::Path::new("/usr/bin/env"),
        &[
            "cargo",
            "build",
            "--release",
            "--manifest-path",
            &manifest_path.to_string_lossy(),
        ],
        target_dir,
        Duration::from_secs(180),
        &[],
        &[
            ("CARGO_TARGET_DIR", &target_dir.to_string_lossy()),
            ("RUSTUP_HOME", &rustup_home),
            ("CARGO_HOME", &cargo_home),
        ],
    );

    let sandbox_result = match result {
        Ok(r) => r,
        Err(e) => {
            return GateResult {
                gate_kind: GateKind::Build,
                passed: false,
                is_candidate_failure: false,
                exit_code: e.exit_code,
                timed_out: false,
                child_cleanup: e.child_cleanup,
                error_code: Some("BUILD_SANDBOX_UNAVAILABLE".into()),
                stdout: e.stdout,
                stderr: e.stderr,
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            };
        }
    };

    let passed = sandbox_result.exit_code == 0 && !sandbox_result.timed_out;

    GateResult {
        gate_kind: GateKind::Build,
        passed,
        is_candidate_failure: !passed && !sandbox_result.timed_out,
        exit_code: sandbox_result.exit_code,
        timed_out: sandbox_result.timed_out,
        child_cleanup: sandbox_result.child_cleanup,
        error_code: if passed {
            None
        } else if sandbox_result.timed_out {
            Some("BUILD_TIMEOUT".into())
        } else {
            Some("BUILD_FAILED".into())
        },
        stdout: sandbox_result.stdout,
        stderr: sandbox_result.stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}

fn copy_candidate_source(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == "target" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == "target" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
