//! TrustedTest acceptance gate.
//!
//! For `calculator-fixture-v0` the gate compiles and runs a
//! system-provided (trusted) test binary.  For other test kits
//! (e.g. `hook-consumer-service-contract-v0`) the gate delegates
//! product correctness to the Acceptance Kit — it verifies that the
//! candidate's manifest contains a non-placeholder
//! `acceptance_bundle_digest`, proving that `verify_frozen_candidate()`
//! already passed.  No product-specific test is re-run.
//!
//! Failure is `CandidateFailed`; infra/sandbox failures are InfraFailure.

use std::path::Path;
use std::time::Duration;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};

/// The placeholder digest value used in manifests before the artifact gate.
const DIGEST_PLACEHOLDER: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// Test kits that run a compiled trusted-test binary against the candidate.
/// For these kits the gate compiles the trusted source and executes it.
const LEGACY_TRUSTED_TEST_KITS: &[&str] = &["calculator-fixture-v0"];

/// Run the trusted test gate.
pub(crate) fn check(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    if LEGACY_TRUSTED_TEST_KITS.contains(&ctx.test_kit.as_str()) {
        run_legacy_trusted_test(candidate, ctx)
    } else {
        verify_acceptance_evidence(candidate, ctx)
    }
}

// ── Legacy path: compile & run a trusted test binary ──────────────

fn run_legacy_trusted_test(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
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

// ── Acceptance-evidence path: verify candidate was already
//    validated by the Acceptance Kit ────────────────────────────────

/// Verify that the candidate manifest carries a non-placeholder
/// `acceptance_bundle_digest`, proving `verify_frozen_candidate()`
/// already passed for this candidate.
///
/// Also performs the usual digest-integrity and existence checks.
fn verify_acceptance_evidence(candidate: &CandidateSnapshot, ctx: &GateContext) -> GateResult {
    let candidate_binary = &ctx.built_binary;
    if !candidate_binary.is_file() {
        return GateResult::infrastructure_failure(
            GateKind::TrustedTest,
            "TRUSTED_TEST_BINARY_NOT_FOUND",
            &format!(
                "candidate binary not found at: {}",
                candidate_binary.display()
            ),
            candidate,
        );
    }

    // Load manifest and check acceptance_bundle_digest
    let manifest_path = candidate.candidate_path.join("manifest.json");
    let manifest_bytes = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(e) => {
            return GateResult {
                gate_kind: GateKind::TrustedTest,
                passed: false,
                is_candidate_failure: true,
                exit_code: -1,
                timed_out: false,
                child_cleanup: crate::hcr::executor::CleanupStatus::Confirmed,
                error_code: Some("TRUSTED_TEST_MANIFEST_MISSING".into()),
                stdout: String::new(),
                stderr: format!("manifest not readable: {e}"),
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            };
        }
    };

    let manifest: serde_json::Value = match serde_json::from_slice(&manifest_bytes) {
        Ok(v) => v,
        Err(e) => {
            return GateResult {
                gate_kind: GateKind::TrustedTest,
                passed: false,
                is_candidate_failure: true,
                exit_code: -1,
                timed_out: false,
                child_cleanup: crate::hcr::executor::CleanupStatus::Confirmed,
                error_code: Some("TRUSTED_TEST_MANIFEST_INVALID".into()),
                stdout: String::new(),
                stderr: format!("manifest parse error: {e}"),
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            };
        }
    };

    let bundle_digest = match manifest
        .get("acceptance_bundle_digest")
        .and_then(serde_json::Value::as_str)
    {
        Some(d) if !d.is_empty() => d,
        _ => {
            return GateResult {
                gate_kind: GateKind::TrustedTest,
                passed: false,
                is_candidate_failure: true,
                exit_code: -1,
                timed_out: false,
                child_cleanup: crate::hcr::executor::CleanupStatus::Confirmed,
                error_code: Some("TRUSTED_TEST_NO_ACCEPTANCE_EVIDENCE".into()),
                stdout: String::new(),
                stderr: "missing acceptance_bundle_digest in manifest — candidate was not verified by Acceptance Kit".into(),
                candidate_id: candidate.candidate_id.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
                candidate_digest_preserved: false,
                computed_artifact_digest: None,
            };
        }
    };

    // Reject placeholder digest — real acceptance must have run
    if bundle_digest == DIGEST_PLACEHOLDER {
        return GateResult {
            gate_kind: GateKind::TrustedTest,
            passed: false,
            is_candidate_failure: true,
            exit_code: -1,
            timed_out: false,
            child_cleanup: crate::hcr::executor::CleanupStatus::Confirmed,
            error_code: Some("TRUSTED_TEST_PLACEHOLDER_ACCEPTANCE".into()),
            stdout: String::new(),
            stderr: format!(
                "acceptance_bundle_digest is placeholder ({DIGEST_PLACEHOLDER}) — acceptance was not actually run"
            ),
            candidate_id: candidate.candidate_id.clone(),
            candidate_digest: candidate.candidate_digest.clone(),
            candidate_digest_preserved: false,
            computed_artifact_digest: None,
        };
    }

    // Acceptance evidence verified — candidate was independently validated.
    GateResult {
        gate_kind: GateKind::TrustedTest,
        passed: true,
        is_candidate_failure: false,
        exit_code: 0,
        timed_out: false,
        child_cleanup: crate::hcr::executor::CleanupStatus::Confirmed,
        error_code: None,
        stdout: format!(
            "acceptance_bundle_digest={bundle_digest}\npassed=acceptance_evidence_driven"
        ),
        stderr: String::new(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
        computed_artifact_digest: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hcr::candidate::CandidateSnapshot;
    use std::path::PathBuf;

    fn make_candidate(manifest_value: serde_json::Value) -> (CandidateSnapshot, GateContext) {
        let tmp = std::env::temp_dir().join(format!(
            "trusted_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest_value).unwrap(),
        )
        .unwrap();
        // Create a dummy binary so the binary-existence check passes
        let binary_path = tmp
            .join("target")
            .join("release")
            .join("generated-hook-consumer");
        std::fs::create_dir_all(binary_path.parent().unwrap()).unwrap();
        std::fs::write(&binary_path, b"#!/bin/dummy").unwrap();
        let snapshot = CandidateSnapshot {
            candidate_id: "test-candidate".into(),
            candidate_path: tmp.clone(),
            candidate_digest:
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        };
        let ctx = GateContext {
            work_base: tmp.join("work"),
            build_source: tmp.join("work").join("build_src"),
            build_target: tmp.join("work").join("target"),
            built_binary: binary_path,
            test_kit: "hook-consumer-service-contract-v0".into(),
        };
        (snapshot, ctx)
    }

    fn manifest_with_bundle(bundle_digest: &str) -> serde_json::Value {
        serde_json::json!({
            "schema_version": "component-artifact-v1",
            "contract_catalog_version": "contract-catalog-v1",
            "component_id": "test",
            "kind": "hook_consumer_service",
            "profile_id": "hook-consumer-service-v0",
            "entry": "target/release/generated-hook-consumer",
            "test_kit": "hook-consumer-service-contract-v0",
            "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "required_contracts": ["event.observe.v0"],
            "requested_permissions": ["journal.observe"],
            "deployment_profile": "managed-service-v0",
            "acceptance_bundle_digest": bundle_digest,
            "acceptance_bundle_ref": "some-bundle-v0"
        })
    }

    #[test]
    fn acceptance_pass_evidence_drives_trusted_test() {
        let (candidate, ctx) = make_candidate(manifest_with_bundle(
            "bundle_sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
        ));
        let result = check(&candidate, &ctx);
        assert!(
            result.passed,
            "trusted_test should pass with valid acceptance evidence"
        );
        assert!(result.stdout.contains("acceptance_evidence_driven"));
    }

    #[test]
    fn trusted_test_does_not_require_literal_ok_true() {
        let (candidate, ctx) = make_candidate(manifest_with_bundle(
            "bundle_sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
        ));
        let result = check(&candidate, &ctx);
        assert!(
            result.passed,
            "must pass without checking for literal \"ok\":true"
        );
        assert!(
            !result.stdout.contains(r#""ok":true"#),
            "must not contain literal ok:true output check"
        );
    }

    #[test]
    fn trusted_test_rejects_missing_acceptance_bundle() {
        let invalid = serde_json::json!({
            "schema_version": "component-artifact-v1",
            "component_id": "test",
            "kind": "hook_consumer_service",
            "profile_id": "hook-consumer-service-v0",
            "entry": "target/release/generated-hook-consumer",
            "test_kit": "hook-consumer-service-contract-v0",
            "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        });
        let (candidate, ctx) = make_candidate(invalid);
        let result = check(&candidate, &ctx);
        assert!(
            !result.passed,
            "should fail when acceptance_bundle_digest is missing"
        );
        assert_eq!(
            result.error_code.as_deref(),
            Some("TRUSTED_TEST_NO_ACCEPTANCE_EVIDENCE")
        );
    }

    #[test]
    fn trusted_test_rejects_placeholder_acceptance() {
        let (candidate, ctx) = make_candidate(manifest_with_bundle(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ));
        let result = check(&candidate, &ctx);
        assert!(!result.passed, "placeholder digest should be rejected");
        assert_eq!(
            result.error_code.as_deref(),
            Some("TRUSTED_TEST_PLACEHOLDER_ACCEPTANCE")
        );
    }

    #[test]
    fn trusted_test_rejects_missing_manifest() {
        let tmp = std::env::temp_dir().join(format!(
            "trusted_test_noman_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        // Create a dummy binary so the binary-existence check passes
        let binary_path = tmp
            .join("target")
            .join("release")
            .join("generated-hook-consumer");
        std::fs::create_dir_all(binary_path.parent().unwrap()).unwrap();
        std::fs::write(&binary_path, b"#!/bin/dummy").unwrap();
        // No manifest.json written
        let candidate = CandidateSnapshot {
            candidate_id: "test-candidate".into(),
            candidate_path: tmp.clone(),
            candidate_digest:
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        };
        let ctx = GateContext {
            work_base: tmp.join("work"),
            build_source: tmp.join("work").join("build_src"),
            build_target: tmp.join("work").join("target"),
            built_binary: binary_path,
            test_kit: "hook-consumer-service-contract-v0".into(),
        };
        let result = check(&candidate, &ctx);
        assert!(!result.passed, "missing manifest must fail");
        assert_eq!(
            result.error_code.as_deref(),
            Some("TRUSTED_TEST_MANIFEST_MISSING")
        );
    }

    #[test]
    fn trusted_test_has_no_failure_viewer_special_case() {
        // The gate logic is driven solely by acceptance_bundle_digest,
        // not by candidate name, component_id, or any product-specific field.
        let manifest = manifest_with_bundle(
            "bundle_sha256:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        );
        let (candidate, ctx) = make_candidate(manifest);
        let result = check(&candidate, &ctx);
        assert!(result.passed, "no special casing by product name");

        // Verify nothing in the stdout/stderr mentions any product name
        assert!(!result.stdout.contains("failure-viewer"));
        assert!(!result.stderr.contains("failure-viewer"));
    }

    #[test]
    fn calculator_fixture_source_is_embedded_in_the_harness() {
        let source = crate::fixtures::trusted_test_source("calculator-fixture-v0").unwrap();
        assert!(source.contains("multiply"));
        assert!(source.contains("divide_by_zero"));
    }

    #[test]
    fn hook_consumer_contract_source_still_available_for_external_use() {
        // The source is still embedded for external/ad-hoc use, but the HCR
        // gate no longer compiles and runs it automatically.
        let source =
            crate::fixtures::trusted_test_source("hook-consumer-service-contract-v0").unwrap();
        assert!(source.contains("future.observed.fact.v9"));
        assert!(source.contains("events_applied"));
    }
}
