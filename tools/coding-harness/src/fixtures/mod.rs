mod calculator;
mod hook_consumer;

use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::Value;
use std::path::Path;

pub const CALCULATOR_TRUSTED_TEST_SOURCE: &str =
    include_str!("../../tests/fixtures/calculator_trusted_test.rs");
pub const HOOK_CONSUMER_TRUSTED_TEST_SOURCE: &str =
    include_str!("../../tests/fixtures/hook_consumer_contract_trusted_test.rs");

pub struct SmokeCase {
    pub input: &'static str,
    pub expected: Value,
    pub args: &'static [&'static str],
    pub allow_additional_fields: bool,
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
    if hook_consumer::supports(request) {
        return Some(hook_consumer::generate(artifact_root, request));
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
        "hook-consumer-service-contract-v0" => validate_hook_consumer_manifest(manifest),
        _ => Err(format!("unknown trusted test kit: {test_kit}")),
    }
}

pub fn trusted_test_source(test_kit: &str) -> Option<&'static str> {
    match test_kit {
        "calculator-fixture-v0" => Some(CALCULATOR_TRUSTED_TEST_SOURCE),
        "hook-consumer-service-contract-v0" => Some(HOOK_CONSUMER_TRUSTED_TEST_SOURCE),
        _ => None,
    }
}

pub fn smoke_case(test_kit: &str) -> Option<SmokeCase> {
    match test_kit {
        "calculator-fixture-v0" => Some(SmokeCase {
            input: r#"{"protocol":"process-harness-v1","operation":"multiply","arguments":{"a":6,"b":7}}"#,
            expected: serde_json::json!({"ok": true, "result": 42}),
            args: &[],
            allow_additional_fields: false,
        }),
        "hook-consumer-service-contract-v0" => Some(SmokeCase {
            input: r#"{"schema_version":"event.observe.v0","next_cursor":1,"has_more":false,"events":[{"schema_version":"event.observe.v0","event_id":"smoke-event","event_kind":"future.observed.fact.v9","occurred_at":"2026-07-15T00:00:00Z","payload":{"unknown":true}}]}"#,
            expected: serde_json::json!({
                "ok": true,
                "schema_version": "hook-consumer-service-contract-v0",
                "events_applied": 1,
                "html_nonempty": true,
                "html_safe": true,
                "html_runtime_metadata": true
            }),
            args: &["--profile-contract-test"],
            allow_additional_fields: true,
        }),
        _ => None,
    }
}

pub fn smoke_output_matches(case: &SmokeCase, output: &str) -> bool {
    let Ok(actual) = serde_json::from_str::<Value>(output.trim()) else {
        return false;
    };
    if !case.allow_additional_fields {
        return actual == case.expected;
    }
    let (Some(actual), Some(expected)) = (actual.as_object(), case.expected.as_object()) else {
        return false;
    };
    expected
        .iter()
        .all(|(key, value)| actual.get(key) == Some(value))
}

fn validate_hook_consumer_manifest(manifest: &Value) -> Result<(), String> {
    for (key, expected) in [
        ("kind", "hook_consumer_service"),
        ("profile_id", "hook-consumer-service-v0"),
        ("deployment_profile", "managed-service-v0"),
        ("entry", "target/release/generated-hook-consumer"),
    ] {
        if manifest.get(key).and_then(Value::as_str) != Some(expected) {
            return Err(format!("hook consumer manifest {key} mismatch"));
        }
    }
    if manifest.get("required_contracts") != Some(&serde_json::json!(["event.observe.v0"]))
        || manifest.get("requested_permissions") != Some(&serde_json::json!(["journal.observe"]))
        || manifest.pointer("/service/version").and_then(Value::as_str) != Some("0.1.0")
        || manifest
            .pointer("/service/healthcheck_path")
            .and_then(Value::as_str)
            != Some("/health")
        || manifest.pointer("/generation/kind").and_then(Value::as_str)
            != Some("request-driven-model-module-v0")
        || manifest
            .pointer("/generation/mutable_surface")
            .and_then(Value::as_array)
            != Some(&vec![Value::String("src/component.rs".into())])
    {
        return Err("hook consumer manifest contract mismatch".into());
    }
    let digest = manifest
        .pointer("/generation/module_digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("hook consumer module digest invalid".into());
    }
    Ok(())
}
