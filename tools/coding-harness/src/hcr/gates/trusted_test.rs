//! TrustedTest acceptance gate.
//!
//! Compiles and runs a system-provided (trusted) test binary against
//! the candidate. The test binary is part of the harness, not the
//! candidate, so the candidate cannot cheat by modifying the test.
//!
//! Tests all four operations plus divide-by-zero.
//! Failure is `CandidateFailed`; infra failures are `InfrastructureFailure`.

use std::path::Path;
use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};

/// Run the trusted test gate.
///
/// 1. Locates the trusted test source from the harness fixtures
/// 2. Compiles it with `rustc` (sandboxed)
/// 3. Runs the compiled test binary against the built candidate binary
/// 4. Returns pass/fail based on test exit code
pub fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    // Find the candidate binary (built by the build gate)
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

    // Find the trusted test source
    let trusted_test_source = find_trusted_test_source();
    if trusted_test_source.is_none() {
        return GateResult::infrastructure_failure(
            GateKind::TrustedTest,
            "TRUSTED_TEST_SOURCE_NOT_FOUND",
            "trusted test source file (calculator_trusted_test.rs) not found",
            candidate,
        );
    }
    let test_source = trusted_test_source.unwrap();

    // Compile the trusted test
    let test_binary = ctx.work_base.join("trusted_test_bin");
    let compile_result = compile_trusted_test(&test_source, &test_binary, ctx);

    if !compile_result.passed {
        return compile_result;
    }

    // Run the trusted test against the candidate binary
    run_trusted_test(&test_binary, &candidate_binary, ctx, candidate)
}

/// Find the candidate binary built by the build gate.
fn find_candidate_binary(ctx: &GateContext) -> String {
    // Check the known build output location
    if ctx.built_binary.exists() {
        return ctx.built_binary.to_string_lossy().to_string();
    }

    // Fallback: check standard release path in build_source
    let alt = ctx.build_source.join("target/release/calculator-harness");
    if alt.exists() {
        return alt.to_string_lossy().to_string();
    }

    ctx.built_binary.to_string_lossy().to_string()
}

/// Locate the trusted test source file relative to this crate's root.
fn find_trusted_test_source() -> Option<String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest_dir.join("tests/fixtures/calculator_trusted_test.rs");
    if candidate.exists() {
        return candidate.to_str().map(|s| s.to_string());
    }
    None
}

/// Compile the trusted test using rustc.
fn compile_trusted_test(source_path: &str, output_path: &Path, ctx: &GateContext) -> GateResult {
    if let Some(parent) = output_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{h}/.rustup"))
            .unwrap_or_default()
    });

    let (exit_code, timed_out, stdout, stderr, _child_cleanup) =
        super::run_command_direct_sandboxed(
            std::path::Path::new("/usr/bin/env"),
            &["rustc", source_path, "-o", &output_path.to_string_lossy()],
            &ctx.work_base,
            Duration::from_secs(120),
            &[],
            &[("RUSTUP_HOME", &rustup_home)],
        );

    let passed = exit_code == 0 && !timed_out;

    GateResult {
        gate_kind: GateKind::TrustedTest,
        passed,
        is_candidate_failure: false,
        exit_code,
        timed_out,
        child_cleanup: crate::hcr::executor::CleanupStatus::Confirmed,
        error_code: if passed {
            None
        } else if timed_out {
            Some("TRUSTED_TEST_COMPILE_TIMEOUT".into())
        } else {
            Some("TRUSTED_TEST_COMPILE_FAILED".into())
        },
        stdout,
        stderr,
        candidate_id: String::new(),
        candidate_digest: String::new(),
        candidate_digest_preserved: false,
    }
}

/// Run the compiled trusted test binary against the candidate.
fn run_trusted_test(
    test_binary: &Path,
    candidate_binary: &str,
    ctx: &GateContext,
    candidate: &CandidateSnapshot,
) -> GateResult {
    let (exit_code, timed_out, stdout, stderr, child_cleanup) = super::run_command_direct_sandboxed(
        test_binary,
        &[candidate_binary],
        &ctx.work_base,
        Duration::from_secs(60),
        &[],
        &[],
    );

    let passed = exit_code == 0 && !timed_out;

    GateResult {
        gate_kind: GateKind::TrustedTest,
        passed,
        is_candidate_failure: !passed && !timed_out,
        exit_code,
        timed_out,
        child_cleanup,
        error_code: if passed {
            None
        } else if timed_out {
            Some("TRUSTED_TEST_TIMEOUT".into())
        } else {
            Some("TRUSTED_TEST_FAILED".into())
        },
        stdout,
        stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
    }
}
