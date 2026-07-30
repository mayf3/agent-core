use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

pub(super) fn check_tests(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let (contract, operation) = match load(candidate) {
        Ok(value) => value,
        Err(error) => {
            return result(
                candidate,
                GateKind::TrustedTest,
                false,
                Some("TRUSTED_TEST_PROFILE_CONTRACT_INVALID"),
                String::new(),
                error,
            )
        }
    };
    let mut evidence = Vec::new();
    for case in &contract.contract_tests {
        let input = crate::self_evolution::invocable_process_input(&operation, &case.arguments);
        let run = match super::run_command_sandboxed(
            &ctx.built_binary,
            &[],
            &ctx.work_base,
            Duration::from_secs(30),
            &[&input],
            &[],
        ) {
            Ok(value) => value,
            Err(error) => return infrastructure(candidate, GateKind::TrustedTest, error),
        };
        let actual = serde_json::from_str::<serde_json::Value>(run.stdout.trim()).ok();
        let expected = serde_json::json!({"ok":true,"result":case.expected_result});
        if run.exit_code != 0 || run.timed_out || actual.as_ref() != Some(&expected) {
            return result(
                candidate,
                GateKind::TrustedTest,
                false,
                Some("TRUSTED_TEST_FAILED"),
                run.stdout,
                format!("contract case failed: {}", case.case_id),
            );
        }
        evidence.push(case.case_id.clone());
    }
    result(
        candidate,
        GateKind::TrustedTest,
        true,
        None,
        format!("contract_cases_passed={}", evidence.join(",")),
        String::new(),
    )
}

pub(super) fn check_smoke(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let (contract, operation) = match load(candidate) {
        Ok(value) => value,
        Err(error) => {
            return result(
                candidate,
                GateKind::TrustedSmoke,
                false,
                Some("SMOKE_PROFILE_CONTRACT_INVALID"),
                String::new(),
                error,
            )
        }
    };
    let input =
        crate::self_evolution::invocable_process_input(&operation, &contract.probe_arguments);
    let run = match super::run_command_sandboxed(
        &ctx.built_binary,
        &[],
        &ctx.work_base,
        Duration::from_secs(30),
        &[&input],
        &[],
    ) {
        Ok(value) => value,
        Err(error) => return infrastructure(candidate, GateKind::TrustedSmoke, error),
    };
    let actual = serde_json::from_str::<serde_json::Value>(run.stdout.trim()).ok();
    let expected = serde_json::json!({"ok":true,"result":contract.probe_result});
    let passed = run.exit_code == 0 && !run.timed_out && actual.as_ref() == Some(&expected);
    result(
        candidate,
        GateKind::TrustedSmoke,
        passed,
        (!passed).then_some("SMOKE_FAILED"),
        run.stdout,
        run.stderr,
    )
}

fn load(
    candidate: &CandidateSnapshot,
) -> Result<(crate::self_evolution::InvocableCapabilityContract, String), String> {
    let contract = crate::self_evolution::InvocableCapabilityContract::load(
        &candidate.candidate_path.join("profile-contract.json"),
    )?;
    let operation = std::fs::read(candidate.candidate_path.join("manifest.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("component_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| "component identity missing".to_string())?;
    Ok((contract, operation))
}

fn infrastructure(
    candidate: &CandidateSnapshot,
    kind: GateKind,
    error: super::SandboxedCommandResult,
) -> GateResult {
    GateResult {
        gate_kind: kind,
        passed: false,
        is_candidate_failure: false,
        exit_code: error.exit_code,
        timed_out: error.timed_out,
        child_cleanup: error.child_cleanup,
        error_code: Some(
            match kind {
                GateKind::TrustedTest => "TRUSTED_TEST_SANDBOX_UNAVAILABLE",
                _ => "SMOKE_SANDBOX_UNAVAILABLE",
            }
            .into(),
        ),
        stdout: error.stdout,
        stderr: error.stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}

fn result(
    candidate: &CandidateSnapshot,
    kind: GateKind,
    passed: bool,
    error_code: Option<&str>,
    stdout: String,
    stderr: String,
) -> GateResult {
    GateResult {
        gate_kind: kind,
        passed,
        is_candidate_failure: !passed,
        exit_code: if passed { 0 } else { 1 },
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: error_code.map(str::to_string),
        stdout,
        stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}
