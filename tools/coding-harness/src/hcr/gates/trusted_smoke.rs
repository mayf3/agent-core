//! TrustedSmoke acceptance gate.
//!
//! Runs the candidate entry point with `multiply(6, 7)` and verifies
//! the output is `42`. Mandatory sandbox execution (B2: fail-closed).
//!
//! Failure is `CandidateFailed`; infra/sandbox failures are InfraFailure.

use std::path::Path;
use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

/// Run the trusted smoke gate.
pub fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let candidate_binary = find_candidate_binary(ctx);

    if !std::fs::metadata(&candidate_binary)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return GateResult {
            gate_kind: GateKind::TrustedSmoke,
            passed: false,
            is_candidate_failure: false, // infra: build pipeline didn't produce binary
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

    let input =
        r#"{"protocol":"process-harness-v1","operation":"multiply","arguments":{"a":6,"b":7}}"#;

    let result = super::run_command_sandboxed(
        std::path::Path::new(&candidate_binary),
        &[],
        &ctx.work_base,
        Duration::from_secs(30),
        &[input],
        &[],
    );

    let sr = match result {
        Ok(r) => r,
        Err(e) => {
            return GateResult {
                gate_kind: GateKind::TrustedSmoke,
                passed: false,
                is_candidate_failure: false,
                exit_code: e.exit_code,
                timed_out: false,
                child_cleanup: e.child_cleanup,
                error_code: Some("SMOKE_SANDBOX_UNAVAILABLE".into()),
                stdout: e.stdout,
                stderr: e.stderr,
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
            };
        }
    };

    let passed = if sr.exit_code == 0 && !sr.timed_out {
        check_smoke_output(&sr.stdout)
    } else {
        false
    };

    GateResult {
        gate_kind: GateKind::TrustedSmoke,
        passed,
        is_candidate_failure: !passed && !sr.timed_out,
        exit_code: sr.exit_code,
        timed_out: sr.timed_out,
        child_cleanup: sr.child_cleanup,
        error_code: if passed {
            None
        } else if sr.timed_out {
            Some("SMOKE_TIMEOUT".into())
        } else {
            Some("SMOKE_FAILED".into())
        },
        stdout: sr.stdout,
        stderr: sr.stderr,
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
    }
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
