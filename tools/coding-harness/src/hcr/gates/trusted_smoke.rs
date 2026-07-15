//! Profile-selected trusted smoke gate.

use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

pub(crate) fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let smoke = match crate::fixtures::smoke_case(&ctx.test_kit) {
        Some(smoke) => smoke,
        None => {
            return failure(
                candidate,
                true,
                "SMOKE_TEST_KIT_UNKNOWN",
                format!("unknown smoke test kit: {}", ctx.test_kit),
            )
        }
    };
    if !ctx.built_binary.is_file() {
        return failure(
            candidate,
            false,
            "SMOKE_BINARY_NOT_FOUND",
            format!("candidate binary not found: {}", ctx.built_binary.display()),
        );
    }
    let result = super::run_command_sandboxed(
        &ctx.built_binary,
        &[],
        &ctx.work_base,
        Duration::from_secs(30),
        &[smoke.input],
        &[],
    );
    let sandbox = match result {
        Ok(result) => result,
        Err(error) => {
            return GateResult {
                gate_kind: GateKind::TrustedSmoke,
                passed: false,
                is_candidate_failure: false,
                exit_code: error.exit_code,
                timed_out: false,
                child_cleanup: error.child_cleanup,
                error_code: Some("SMOKE_SANDBOX_UNAVAILABLE".into()),
                stdout: error.stdout,
                stderr: error.stderr,
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            }
        }
    };
    let passed = sandbox.exit_code == 0
        && !sandbox.timed_out
        && crate::fixtures::smoke_output_matches(&smoke, &sandbox.stdout);
    GateResult {
        gate_kind: GateKind::TrustedSmoke,
        passed,
        is_candidate_failure: !passed && !sandbox.timed_out,
        exit_code: sandbox.exit_code,
        timed_out: sandbox.timed_out,
        child_cleanup: sandbox.child_cleanup,
        error_code: if passed {
            None
        } else if sandbox.timed_out {
            Some("SMOKE_TIMEOUT".into())
        } else {
            Some("SMOKE_FAILED".into())
        },
        stdout: sandbox.stdout,
        stderr: sandbox.stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}

fn failure(
    candidate: &CandidateSnapshot,
    candidate_failure: bool,
    code: &str,
    message: String,
) -> GateResult {
    GateResult {
        gate_kind: GateKind::TrustedSmoke,
        passed: false,
        is_candidate_failure: candidate_failure,
        exit_code: -1,
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: Some(code.into()),
        stdout: String::new(),
        stderr: message,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn calculator_smoke_is_a_fixture_case() {
        let smoke = crate::fixtures::smoke_case("calculator-fixture-v0").unwrap();
        assert!(crate::fixtures::smoke_output_matches(
            &smoke,
            r#"{"ok":true,"result":42}"#
        ));
        assert!(!crate::fixtures::smoke_output_matches(
            &smoke,
            r#"{"ok":true,"result":41}"#
        ));
    }
}
