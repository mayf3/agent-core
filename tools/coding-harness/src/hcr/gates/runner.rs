//! Ordered five-gate runner with candidate digest enforcement.

use super::{artifact, build, scaffold, trusted_smoke, trusted_test, GateKind, GateResult};
use crate::hcr::candidate::{verify_digest, CandidateSnapshot};
use std::path::PathBuf;

/// Shared context passed between gates during execution.
#[derive(Debug, Clone)]
pub(crate) struct GateContext {
    pub work_base: PathBuf,
    pub build_source: PathBuf,
    pub build_target: PathBuf,
    pub built_binary: PathBuf,
}

impl GateContext {
    pub fn new(work_base: PathBuf, _candidate: &CandidateSnapshot) -> Self {
        Self {
            build_source: work_base.join("build_src"),
            build_target: work_base.join("target"),
            built_binary: work_base.join("target/release/calculator-harness"),
            work_base,
        }
    }
}

/// Error emitted when immutable candidate content changes during acceptance.
pub const CANDIDATE_INTEGRITY_VIOLATION: &str = "CANDIDATE_INTEGRITY_VIOLATION";

/// Gate results plus the exact verified executable bytes, when all gates pass.
pub(crate) struct AcceptanceGateRun {
    pub results: Vec<GateResult>,
    pub artifact_bytes: Option<Vec<u8>>,
}

pub fn run_all_gates(candidate: &CandidateSnapshot) -> Vec<GateResult> {
    run(candidate).results
}

pub(crate) fn run_all_gates_for_acceptance(candidate: &CandidateSnapshot) -> AcceptanceGateRun {
    run(candidate)
}

fn run(candidate: &CandidateSnapshot) -> AcceptanceGateRun {
    let expected_digest = &candidate.candidate_digest;
    let work_base = std::env::temp_dir().join(format!(
        "hcr_gates_work_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let ctx = GateContext::new(work_base.clone(), candidate);
    let run_gate =
        |gate_kind: GateKind, gate_fn: fn(&CandidateSnapshot, &GateContext) -> GateResult| {
            if !verify_digest(candidate).unwrap_or(false) {
                return integrity_failure(candidate, gate_kind, expected_digest, "before");
            }
            let mut result = gate_fn(candidate, &ctx);
            if !verify_digest(candidate).unwrap_or(false) {
                return integrity_failure(candidate, gate_kind, expected_digest, "during/after");
            }
            result.candidate_digest_preserved = true;
            result.candidate_id = candidate.candidate_id.clone();
            result.candidate_digest = candidate.candidate_digest.clone();
            result
        };
    let gates: [(GateKind, fn(&CandidateSnapshot, &GateContext) -> GateResult); 5] = [
        (GateKind::Scaffold, scaffold::check),
        (GateKind::Build, build::check),
        (GateKind::TrustedTest, trusted_test::check),
        (GateKind::TrustedSmoke, trusted_smoke::check),
        (GateKind::Artifact, artifact::check),
    ];
    let mut results = Vec::with_capacity(5);
    for (kind, check) in gates {
        let result = run_gate(kind, check);
        let abort =
            !result.passed && result.error_code.as_deref() == Some(CANDIDATE_INTEGRITY_VIOLATION);
        results.push(result);
        if abort {
            break;
        }
    }
    let all_passed = results.len() == 5 && results.iter().all(|result| result.passed);
    let artifact_bytes = all_passed
        .then(|| std::fs::read(&ctx.built_binary).ok())
        .flatten();
    let _ = std::fs::remove_dir_all(&work_base);
    AcceptanceGateRun {
        results,
        artifact_bytes,
    }
}

fn integrity_failure(
    candidate: &CandidateSnapshot,
    kind: GateKind,
    expected: &str,
    phase: &str,
) -> GateResult {
    let mut result = GateResult::infrastructure_failure(
        kind,
        CANDIDATE_INTEGRITY_VIOLATION,
        &format!(
            "candidate digest changed {phase} {} gate: expected {expected}",
            kind.as_str()
        ),
        candidate,
    );
    result.candidate_digest_preserved = false;
    result
}
