//! Scaffold acceptance gate.
//!
//! Verifies that the candidate has the expected directory structure,
//! a parseable manifest, declares `external.calculator` with the four
//! basic operations, and that the entry point is syntactically valid.
//!
//! Failure is always `CandidateFailed` (not `InfrastructureFailure`).

use std::path::Path;

use super::{CandidateSnapshot, GateContext, GateKind, GateResult};
use crate::hcr::executor::CleanupStatus;

/// Run the scaffold gate against the given candidate snapshot.
///
/// Checks:
/// 1. `Cargo.toml` exists and is parseable
/// 2. `src/main.rs` exists
/// 3. `manifest.json` exists and is valid JSON
/// 4. manifest declares `operation = "external.calculator"`
/// 5. manifest declares add/subtract/multiply/divide operations
/// 6. entry path from manifest is syntactically valid
pub fn check(candidate: &CandidateSnapshot, _ctx: &GateContext) -> GateResult {
    let candidate_path = &candidate.candidate_path;
    let mut errors: Vec<String> = Vec::new();

    // 1. Check Cargo.toml
    let cargo_toml = candidate_path.join("Cargo.toml");
    if !cargo_toml.exists() {
        errors.push("Cargo.toml not found".into());
    } else if !cargo_toml.is_file() {
        errors.push("Cargo.toml is not a file".into());
    } else {
        let content = match std::fs::read_to_string(&cargo_toml) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("Cargo.toml read error: {e}"));
                String::new()
            }
        };
        if !content.is_empty() && !content.contains("[package]") {
            errors.push("Cargo.toml missing [package] section".into());
        }
    }

    // 2. Check src/main.rs
    let src_main = candidate_path.join("src/main.rs");
    if !src_main.exists() {
        errors.push("src/main.rs not found".into());
    } else if !src_main.is_file() {
        errors.push("src/main.rs is not a file".into());
    }

    // 3. Check and parse manifest.json
    let manifest_path = candidate_path.join("manifest.json");
    let manifest = if !manifest_path.exists() {
        errors.push("manifest.json not found".into());
        None
    } else if !manifest_path.is_file() {
        errors.push("manifest.json is not a file".into());
        None
    } else {
        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("manifest.json read error: {e}"));
                String::new()
            }
        };
        if content.is_empty() {
            errors.push("manifest.json is empty".into());
            None
        } else {
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(v) => Some(v),
                Err(e) => {
                    errors.push(format!("manifest.json parse error: {e}"));
                    None
                }
            }
        }
    };

    // 4. Check operation = external.calculator
    if let Some(ref m) = manifest {
        let op = m["operation"].as_str().unwrap_or("");
        if op != "external.calculator" {
            errors.push(format!(
                "manifest operation is '{op}', expected 'external.calculator'"
            ));
        }
    }

    // 5. Check operations list contains add/subtract/multiply/divide
    if let Some(ref m) = manifest {
        let ops = m["operations"].as_array();
        let expected = ["add", "subtract", "multiply", "divide"];
        match ops {
            Some(arr) => {
                let declared: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                for expected_op in &expected {
                    if !declared.contains(expected_op) {
                        errors.push(format!(
                            "manifest missing required operation: {expected_op}"
                        ));
                    }
                }
            }
            None => {
                errors.push("manifest missing 'operations' array".into());
            }
        }
    }

    // 6. Check entry path is syntactically valid
    if let Some(ref m) = manifest {
        if let Some(entry) = m["entry"].as_str() {
            if Path::new(entry).is_absolute() {
                errors.push(format!("entry path is absolute: {entry}"));
            }
            if entry.contains("..") {
                errors.push(format!("entry path contains '..': {entry}"));
            }
        }
    }

    let passed = errors.is_empty();
    GateResult {
        gate_kind: GateKind::Scaffold,
        passed,
        is_candidate_failure: !passed,
        exit_code: if passed { 0 } else { -1 },
        timed_out: false,
        child_cleanup: CleanupStatus::Confirmed,
        error_code: if passed {
            None
        } else {
            Some("SCAFFOLD_FAILED".into())
        },
        stdout: String::new(),
        stderr: errors.join("\n"),
        candidate_id: candidate.candidate_id.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
        candidate_digest_preserved: false,
    computed_artifact_digest: None,
    }
}
