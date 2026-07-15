//! TrustedTest acceptance gate.
//!
//! Compiles and runs a system-provided (trusted) test binary against
//! the candidate. The test binary is part of the harness, not the
//! candidate, so the candidate cannot cheat by modifying the test.
//!
//! The selected Component Profile fixture supplies the trusted test kit.
//! Failure is `CandidateFailed`; infra/sandbox failures are InfraFailure.

use std::path::Path;
use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};

/// Run the trusted test gate.
pub(crate) fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let candidate_binary = find_candidate_binary(ctx);
    let trusted_source = match crate::fixtures::trusted_test_source(&ctx.test_kit) {
        Some(source) => source,
        None => {
            return GateResult {
                gate_kind: GateKind::TrustedTest,
                passed: false,
                is_candidate_failure: true,
                exit_code: -1,
                timed_out: false,
                child_cleanup: crate::hcr::executor::CleanupStatus::Confirmed,
                error_code: Some("TRUSTED_TEST_KIT_UNKNOWN".into()),
                stdout: String::new(),
                stderr: format!("unknown trusted test kit: {}", ctx.test_kit),
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            }
        }
    };

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
    let compile_result = compile_trusted_test(&test_binary, trusted_source, ctx);
    if !compile_result.passed {
        return compile_result;
    }

    run_trusted_test(&test_binary, &candidate_binary, ctx, candidate)
}

fn find_candidate_binary(ctx: &GateContext) -> String {
    ctx.built_binary.to_string_lossy().to_string()
}

fn compile_trusted_test(output_path: &Path, source: &str, ctx: &GateContext) -> GateResult {
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
        &[source],
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
    #[test]
    fn calculator_fixture_source_is_embedded_in_the_harness() {
        let source = crate::fixtures::trusted_test_source("calculator-fixture-v0").unwrap();
        assert!(source.contains("multiply"));
        assert!(source.contains("divide_by_zero"));
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
