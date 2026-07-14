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

// Embed the trusted source in the Harness binary. Production releases live
// under /home, which bubblewrap intentionally replaces with a private tmpfs;
// passing the checkout path to rustc would therefore make a valid gate fail
// with ENOENT. Feeding this compile-time-controlled source over stdin keeps
// the host checkout and the user's home outside the sandbox.
const TRUSTED_TEST_SOURCE: &str =
    include_str!("../../../tests/fixtures/calculator_trusted_test.rs");

/// Run the trusted test gate.
pub(crate) fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
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

    let test_binary = ctx.work_base.join("trusted_test_bin");
    let compile_result = compile_trusted_test(&test_binary, ctx);
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

fn compile_trusted_test(output_path: &Path, ctx: &GateContext) -> GateResult {
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
        &["rustc", "-", "-o", &output_path.to_string_lossy()],
        &ctx.work_base,
        Duration::from_secs(120),
        &[TRUSTED_TEST_SOURCE],
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

#[cfg(test)]
mod tests {
    use super::TRUSTED_TEST_SOURCE;

    #[test]
    fn trusted_test_source_is_embedded_in_the_harness() {
        assert!(TRUSTED_TEST_SOURCE.contains("multiply"));
        assert!(TRUSTED_TEST_SOURCE.contains("divide"));
        assert!(TRUSTED_TEST_SOURCE.contains("divide_by_zero"));
        assert!(!TRUSTED_TEST_SOURCE.trim().is_empty());
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
