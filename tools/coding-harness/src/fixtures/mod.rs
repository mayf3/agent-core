mod calculator;

use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::Value;
use std::path::Path;

pub const CALCULATOR_TRUSTED_TEST_SOURCE: &str =
    include_str!("../../tests/fixtures/calculator_trusted_test.rs");

pub struct SmokeCase {
    pub input: &'static str,
    pub expected: Value,
}

/// Ordinary deterministic fixtures share the same Generic DevelopmentRequest
/// entrypoint as generated components. A fixture may claim only requests that
/// match its catalogued profile and immutable identity.
pub fn generate(
    artifact_root: &Path,
    request: &DevelopmentRequest,
) -> Option<Result<Value, std::io::Error>> {
    if calculator::supports(request) {
        return Some(calculator::generate(artifact_root, request));
    }
    None
}

pub fn validate_manifest(test_kit: &str, manifest: &Value) -> Result<(), String> {
    match test_kit {
        "calculator-fixture-v0" => {
            if manifest.get("component_id").and_then(Value::as_str) != Some("external.calculator")
                || manifest.get("operation").and_then(Value::as_str) != Some("external.calculator")
            {
                return Err("calculator fixture identity mismatch".into());
            }
            let operations: Vec<&str> = manifest
                .get("operations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if operations != ["add", "subtract", "multiply", "divide"] {
                return Err("calculator fixture operation set mismatch".into());
            }
            Ok(())
        }
        _ => Err(format!("unknown trusted test kit: {test_kit}")),
    }
}

pub fn trusted_test_source(test_kit: &str) -> Option<&'static str> {
    match test_kit {
        "calculator-fixture-v0" => Some(CALCULATOR_TRUSTED_TEST_SOURCE),
        _ => None,
    }
}

pub fn smoke_case(test_kit: &str) -> Option<SmokeCase> {
    match test_kit {
        "calculator-fixture-v0" => Some(SmokeCase {
            input: r#"{"protocol":"process-harness-v1","operation":"multiply","arguments":{"a":6,"b":7}}"#,
            expected: serde_json::json!({"ok": true, "result": 42}),
        }),
        _ => None,
    }
}

pub fn smoke_output_matches(case: &SmokeCase, output: &str) -> bool {
    serde_json::from_str::<Value>(output.trim())
        .map(|actual| actual == case.expected)
        .unwrap_or(false)
}
