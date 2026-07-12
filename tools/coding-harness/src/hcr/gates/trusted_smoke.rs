//! TrustedSmoke acceptance gate.
//!
//! Runs the candidate entry point with `multiply(6, 7)` and verifies
//! the output is `42`. This is a true smoke test: it starts the
//! candidate process, pipes real input, and parses real output.
//!
//! Failure is `CandidateFailed`; infra failures are `InfrastructureFailure`.

use std::path::Path;
use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

/// Run the trusted smoke gate.
///
/// Pipes `multiply(6, 7)` to the candidate binary, parses the JSON
/// output, and asserts `result == 42`.
pub fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let candidate_binary = find_candidate_binary(ctx);

    if !std::fs::metadata(&candidate_binary)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return GateResult {
            gate_kind: GateKind::TrustedSmoke,
            passed: false,
            is_candidate_failure: true,
            exit_code: -1,
            timed_out: false,
            child_cleanup: CleanupStatus::Confirmed,
            error_code: Some("SMOKE_BINARY_NOT_FOUND".into()),
            stdout: String::new(),
            stderr: format!("candidate binary not found: {candidate_binary}"),
            candidate_id: candidate.candidate_id.clone(),
            candidate_digest: candidate.candidate_digest.clone(),
            candidate_digest_preserved: false,
        };
    }

    // Pipe multiply(6,7) input to the candidate
    let input =
        r#"{"protocol":"process-harness-v1","operation":"multiply","arguments":{"a":6,"b":7}}"#;

    let (exit_code, timed_out, stdout, stderr, child_cleanup) = super::run_command_direct_sandboxed(
        std::path::Path::new(&candidate_binary),
        &[],
        &ctx.work_base,
        Duration::from_secs(30),
        &[input],
        &[],
    );

    // Parse the output and check for 42
    let passed = if exit_code == 0 && !timed_out {
        check_smoke_output(&stdout)
    } else {
        false
    };

    GateResult {
        gate_kind: GateKind::TrustedSmoke,
        passed,
        is_candidate_failure: !passed && !timed_out,
        exit_code,
        timed_out,
        child_cleanup,
        error_code: if passed {
            None
        } else if timed_out {
            Some("SMOKE_TIMEOUT".into())
        } else {
            Some("SMOKE_FAILED".into())
        },
        stdout: stdout.clone(),
        stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
    }
}

/// Find the candidate binary built by the build gate.
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

/// Check the smoke test output for multiply(6,7) = 42.
fn check_smoke_output(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return false;
    }

    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return false,
    };

    if !parsed["ok"].as_bool().unwrap_or(false) {
        return false;
    }

    if let Some(result) = parsed["result"].as_i64() {
        result == 42
    } else if let Some(result) = parsed["result"].as_f64() {
        (result - 42.0).abs() < 0.001
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_output_42_passes() {
        assert!(check_smoke_output(r#"{"ok":true,"result":42}"#));
        assert!(check_smoke_output(r#"{"ok":true,"result":42.0}"#));
    }

    #[test]
    fn smoke_output_wrong_value_fails() {
        assert!(!check_smoke_output(r#"{"ok":true,"result":41}"#));
        assert!(!check_smoke_output(r#"{"ok":true,"result":0}"#));
    }

    #[test]
    fn smoke_output_error_fails() {
        assert!(!check_smoke_output(
            r#"{"ok":false,"error":{"code":"divide_by_zero"}}"#
        ));
    }

    #[test]
    fn smoke_output_empty_fails() {
        assert!(!check_smoke_output(""));
        assert!(!check_smoke_output("not json"));
    }
}
