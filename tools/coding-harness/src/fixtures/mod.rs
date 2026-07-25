mod calculator;
#[cfg(feature = "test-fixtures")]
pub mod hook_consumer;

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
    /// Frozen evaluation time passed as `AGENT_CORE_CONTRACT_EVALUATION_TIME_UTC`.
    /// `None` means the candidate does not require a frozen time for this smoke test.
    pub evaluation_time_utc: Option<&'static str>,
}

/// Ordinary deterministic fixtures share the same Generic DevelopmentRequest
/// entrypoint as generated components. A fixture may claim only requests that
/// match its catalogued profile and immutable identity.
///
/// # Production safety
///
/// Only the calculator fixture is available in release builds. The hook-consumer
/// fixture is gated behind the `test-fixtures` feature (Cargo.toml) so that
/// real Token Dashboard requests always go through the model generator and
/// never receive a pre-built fixture candidate. Gate infrastructure (manifest
/// validation, trusted-test source, smoke case) remains unconditionally
/// available because it is used by the formal gate pipeline.
pub fn generate(
    artifact_root: &Path,
    request: &DevelopmentRequest,
) -> Option<Result<Value, std::io::Error>> {
    if calculator::supports(request) {
        return Some(calculator::generate(artifact_root, request));
    }
    #[cfg(feature = "test-fixtures")]
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
        "invocable-capability-contract-v0" => validate_invocable_manifest(manifest),
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
            evaluation_time_utc: None,
        }),
        "hook-consumer-service-contract-v0" => Some(SmokeCase {
            input: r#"{"schema_version":"event.observe.v0","next_cursor":1,"has_more":false,"events":[{"schema_version":"event.observe.v0","event_id":"smoke-event","event_kind":"future.observed.fact.v9","occurred_at":"2026-07-15T00:00:00Z","payload":{"unknown":true}}]}"#,
            expected: serde_json::json!({
                "schema_version": "hook-consumer-service-contract-v0",
                "events_applied": 1,
                "html_nonempty": true,
                "html_safe": true,
                "html_runtime_metadata": true
            }),
            args: &["--profile-contract-test"],
            allow_additional_fields: true,
            evaluation_time_utc: Some("2026-07-15T12:00:00Z"),
        }),
        "invocable-capability-contract-v0" => Some(SmokeCase {
            input: r#"{"protocol_version":"process-harness-v1","operation_name":"external.failure_viewer_query","arguments":{"__agent_core_upstream_state":{"rendered":{"component_id":"failure-viewer","component_version":"0.1.2","health":"ready","failure_count":2,"failure_events":[{"capability_name":"external.alpha","failed_stage":"external_execution","error_category":"timeout","detail_code":"UPSTREAM_TIMEOUT","run_id":"run-alpha","invocation_id":"inv-1","receipt_status":"Failed","receipt_time":"2026-07-15T10:00:00Z"},{"capability_name":"external.coding_task_submit","failed_stage":"external_execution","error_category":"external_configuration_missing","detail_code":"GENERATOR_NOT_CONFIGURED_FOR_PROFILE","run_id":"run-beta","invocation_id":"inv-2","receipt_status":"Failed","receipt_time":"2026-07-16T14:30:00Z"}]}}}}"#,
            expected: serde_json::json!({
                "ok": true,
                "result": {
                    "capability_name": "external.coding_task_submit",
                    "failed_stage": "external_execution",
                    "error_category": "external_configuration_missing",
                    "detail_code": "GENERATOR_NOT_CONFIGURED_FOR_PROFILE",
                    "run_id": "run-beta",
                    "invocation_id": "inv-2",
                    "receipt_status": "Failed",
                    "receipt_time": "2026-07-16T14:30:00Z",
                    "source_component": "failure-viewer"
                }
            }),
            args: &[],
            allow_additional_fields: false,
            evaluation_time_utc: None,
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

fn validate_invocable_manifest(manifest: &Value) -> Result<(), String> {
    for (key, expected) in [
        ("kind", "invocable_capability"),
        ("profile_id", "invocable-capability-v0"),
        ("deployment_profile", "capability-host-v0"),
        ("runtime_profile", "process-harness-v1"),
        ("entry", "target/release/generated-invocable-capability"),
    ] {
        if manifest.get(key).and_then(Value::as_str) != Some(expected) {
            return Err(format!("invocable manifest {key} mismatch"));
        }
    }
    if manifest.get("required_contracts") != Some(&serde_json::json!(["component.invoke.v0"]))
        || manifest.get("requested_permissions") != Some(&serde_json::json!(["component.invoke"]))
        || manifest.pointer("/capability/operation_name") != manifest.get("component_id")
        || manifest.pointer("/capability/input_schema")
            != Some(&serde_json::json!({
                "type":"object","properties":{},"required":[],"additionalProperties":false
            }))
        || manifest
            .pointer("/capability/idempotent")
            .and_then(Value::as_bool)
            != Some(true)
        || manifest.pointer("/generation/kind").and_then(Value::as_str)
            != Some("request-driven-model-transform-v0")
        || manifest
            .pointer("/generation/mutable_surface")
            .and_then(Value::as_array)
            != Some(&vec![Value::String("src/component.rs".into())])
    {
        return Err("invocable manifest contract mismatch".into());
    }
    let digest = manifest
        .pointer("/generation/module_digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("invocable module digest invalid".into());
    }
    Ok(())
}
