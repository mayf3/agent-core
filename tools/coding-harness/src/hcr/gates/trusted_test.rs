//! TrustedTest acceptance gate.
//!
//! Compiles and runs a system-provided (trusted) test binary against
//! the candidate. The test binary is part of the harness, not the
//! candidate, so the candidate cannot cheat by modifying the test.
//!
//! Tests all four operations plus divide-by-zero.
//! Failure is `CandidateFailed`; infra/sandbox failures are InfraFailure.

use std::path::Path;
use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

/// Run the trusted test gate.
pub fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let candidate_binary = find_candidate_binary(ctx);

    if !std::fs::metadata(&candidate_binary)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return GateResult::infrastructure_failure(
            GateKind::TrustedTest,
            "TRUSTED_TEST_BINARY_NOT_FOUND",
            &format!("candidate binary not found at: {candidate_binary}"),
            candidate,
        );
    }

    let trusted_test_source = find_trusted_test_source();
    if trusted_test_source.is_none() {
        return GateResult::infrastructure_failure(
            GateKind::TrustedTest,
            "TRUSTED_TEST_SOURCE_NOT_FOUND",
            "trusted test source not found",
            candidate,
        );
    }
    let test_source = trusted_test_source.unwrap();

    let test_binary = ctx.work_base.join("trusted_test_bin");
    let compile_result = compile_trusted_test(&test_source, &test_binary, ctx);
    if !compile_result.passed {
        return compile_result;
    }

    run_trusted_test(&test_binary, &candidate_binary, ctx, candidate)
}

fn find_candidate_binary(ctx: &GateContext) -> String {
    if ctx.built_binary.exists() {
        return ctx.built_binary.to_string_lossy().to_string();
    }
    let alt = ctx.build_source.join("target/release/calculator-harness");
    if alt.exists() {
        return alt.to_string_lossy().to_string();
    }
    ctx.built_binary.to_string_lossy().to_string()
}

fn find_trusted_test_source() -> Option<String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest_dir.join("tests/fixtures/calculator_trusted_test.rs");
    if candidate.exists() {
        return candidate.to_str().map(|s| s.to_string());
    }
    None
}

fn compile_trusted_test(source_path: &str, output_path: &Path, ctx: &GateContext) -> GateResult {
    if let Some(parent) = output_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{h}/.rustup"))
            .unwrap_or_default()
    });

    let result = super::run_command_sandboxed(
        std::path::Path::new("/usr/bin/env"),
        &["rustc", source_path, "-o", &output_path.to_string_lossy()],
        &ctx.work_base,
        Duration::from_secs(120),
        &[],
        &[("RUSTUP_HOME", &rustup_home)],
    );

    let sr = match result {
        Ok(r) => r,
        Err(e) => {
            return GateResult {
                gate_kind: GateKind::TrustedTest,
                passed: false,
                is_candidate_failure: false,
                exit_code: e.exit_code,
                timed_out: false,
                child_cleanup: e.child_cleanup,
                error_code: Some("TRUSTED_TEST_SANDBOX_UNAVAILABLE".into()),
                stdout: e.stdout,
                stderr: e.stderr,
                candidate_id: String::new(),
                candidate_digest: String::new(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            };
        }
    };

    let passed = sr.exit_code == 0 && !sr.timed_out;
    GateResult {
        gate_kind: GateKind::TrustedTest,
        passed,
        is_candidate_failure: false,
        exit_code: sr.exit_code,
        timed_out: sr.timed_out,
        child_cleanup: sr.child_cleanup,
        error_code: if passed {
            None
        } else if sr.timed_out {
            Some("TRUSTED_TEST_COMPILE_TIMEOUT".into())
        } else {
            Some("TRUSTED_TEST_COMPILE_FAILED".into())
        },
        stdout: sr.stdout,
        stderr: sr.stderr,
        candidate_id: String::new(),
        candidate_digest: String::new(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}

fn run_trusted_test(
    test_binary: &Path,
    candidate_binary: &str,
    ctx: &GateContext,
    candidate: &CandidateSnapshot,
) -> GateResult {
    let result = super::run_command_sandboxed(
        test_binary,
        &[candidate_binary],
        &ctx.work_base,
        Duration::from_secs(60),
        &[],
        &[],
    );

    let sr = match result {
        Ok(r) => r,
        Err(e) => {
            return GateResult {
                gate_kind: GateKind::TrustedTest,
                passed: false,
                is_candidate_failure: false,
                exit_code: e.exit_code,
                timed_out: false,
                child_cleanup: e.child_cleanup,
                error_code: Some("TRUSTED_TEST_SANDBOX_UNAVAILABLE".into()),
                stdout: e.stdout,
                stderr: e.stderr,
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            };
        }
    };

    let passed = sr.exit_code == 0 && !sr.timed_out;
    GateResult {
        gate_kind: GateKind::TrustedTest,
        passed,
        is_candidate_failure: !passed && !sr.timed_out,
        exit_code: sr.exit_code,
        timed_out: sr.timed_out,
        child_cleanup: sr.child_cleanup,
        error_code: if passed {
            None
        } else if sr.timed_out {
            Some("TRUSTED_TEST_TIMEOUT".into())
        } else {
            Some("TRUSTED_TEST_FAILED".into())
        },
        stdout: sr.stdout,
        stderr: sr.stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}
