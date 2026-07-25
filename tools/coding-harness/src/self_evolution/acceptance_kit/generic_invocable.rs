//! Generic invocable-capability acceptance kit.
//!
//! Used when no specific acceptance kit matches the component name.
//! Verifies that the generated candidate:
//! - Produces valid JSON output
//! - Correctly handles the required arguments from the development request
//! - Returns output that satisfies the requested output schema
//!
//! No upstream component is required — the fixture provides a minimal
//! JSON document to exercise the transform function.

use super::PrivateVerificationCase;
use serde_json::Value;

/// Public specification shown to the model during generation.
pub fn public_spec() -> Value {
    serde_json::json!({
        "kit_id": "generic-invocable-capability-v0",
        "kit_version": "v0",
        "target_profile": "invocable-capability-v0",
        "description": "Generic invocable capability acceptance kit. Applies to any invocable-capability-v0 component that does not have a specialized acceptance kit.",
        "transform_contract": {
            "input": "The generated `transform(upstream: &Value) -> Value` function receives a trusted JSON Value (the fixture input for verification, empty at runtime). The function should ignore or process the upstream value as needed. The actual user-facing arguments are provided by the Capability Host at runtime via the deployment request.",
            "output": "Return a deterministic JSON Value that satisfies the acceptance criteria of the development request. The output must be valid JSON and must conform to the requested output behavior described in the development request's acceptance_criteria."
        },
        "output_json_schema": {
            "type": ["object", "array", "string", "number", "boolean", "null"],
            "description": "Any valid JSON value. The specific structure depends on the development request."
        }
    })
}

/// A minimal fixture for testing generic invocable capabilities.
/// Provides a basic JSON document to exercise the transform function.
pub fn fixture() -> Value {
    serde_json::json!({
        "test_document": {
            "key": "value",
            "nested": {
                "inner": 42
            },
            "items": [1, 2, 3]
        }
    })
}

/// Private verification cases for the generic kit.
pub fn private_verification_cases() -> &'static [PrivateVerificationCase] {
    &[PrivateVerificationCase {
        case_id: "generic-invocable-transform-valid-json",
        input: r#"{"events":[{"event_kind":"model.invocation.completed.v0","input_tokens":100,"output_tokens":50}]}"#,
        evaluation_time_utc: "2026-07-25T00:00:00Z",
    }]
}

/// Verify that the generated candidate produces valid JSON output.
pub fn verify(
    _request: &agent_core_kernel::domain::DevelopmentRequest,
    _source: &str,
    _input: &str,
    stdout: &str,
) -> Result<(), String> {
    // Parse the output as JSON to verify it's valid
    let output: Value =
        serde_json::from_str(stdout).map_err(|e| format!("OUTPUT_JSON_INVALID: {e}"))?;

    // Verify the response structure
    let ok = output
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "OUTPUT_MISSING_OK_FIELD".to_string())?;
    if !ok {
        let error = output
            .get("error")
            .or_else(|| output.get("error_code"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(format!("OUTPUT_REPORTED_FAILURE: {error}"));
    }

    // Verify result is present
    let _result = output
        .get("result")
        .ok_or_else(|| "OUTPUT_MISSING_RESULT_FIELD".to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
    use agent_core_kernel::domain::{DevelopmentRequest, DevelopmentRequestDraft, TargetKind};

    fn test_request() -> agent_core_kernel::domain::DevelopmentRequest {
        let mut draft =
            DevelopmentRequestDraft::new(TargetKind::InvocableCapability, "external.json_select".into());
        draft.requirements = vec!["Return the value at a given path in a JSON document.".into()];
        draft.required_contracts = vec!["component.invoke.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["The output must contain the selected value or null if not found.".into()];
        draft.build_profile = "invocable-capability-v0".into();
        draft.deployment_profile = "capability-host-v0".into();
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "scope:test".into(),
            "message:test".into(),
            "development:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    #[test]
    fn public_spec_is_valid_json() {
        let spec = public_spec();
        assert!(spec.get("kit_id").and_then(Value::as_str).is_some());
        assert!(spec.get("target_profile").and_then(Value::as_str) == Some("invocable-capability-v0"));
    }

    #[test]
    fn fixture_is_valid_json() {
        let f = fixture();
        assert!(f.get("test_document").is_some());
    }

    #[test]
    fn verify_accepts_valid_output() {
        let result = verify(
            &test_request(),
            "fn transform(upstream: &Value) -> Value { json!({}) }",
            r#"{}"#,
            r#"{"ok":true,"result":{"found":true,"value":42}}"#,
        );
        assert!(result.is_ok(), "valid output should pass: {:?}", result);
    }

    #[test]
    fn verify_rejects_invalid_json_output() {
        let result = verify(
            &test_request(),
            "fn transform(upstream: &Value) -> Value { json!({}) }",
            r#"{}"#,
            r#"this is not json"#,
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("OUTPUT_JSON_INVALID"),
            "should report invalid JSON"
        );
    }

    #[test]
    fn verify_rejects_output_without_ok() {
        let result = verify(
            &test_request(),
            "fn transform(upstream: &Value) -> Value { json!({}) }",
            r#"{}"#,
            r#"{"result":42}"#,
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("OUTPUT_MISSING_OK_FIELD"),
            "should report missing ok field"
        );
    }

    #[test]
    fn verify_rejects_failed_output() {
        let result = verify(
            &test_request(),
            "fn transform(upstream: &Value) -> Value { json!({}) }",
            r#"{}"#,
            r#"{"ok":false,"error_code":"something_wrong"}"#,
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("OUTPUT_REPORTED_FAILURE"),
            "should report output failure"
        );
    }

    #[test]
    fn private_cases_have_valid_input() {
        for case in private_verification_cases() {
            assert!(!case.case_id.is_empty());
            let parsed: Result<Value, _> = serde_json::from_str(case.input);
            assert!(
                parsed.is_ok(),
                "case '{}' input is not valid JSON: {:?}",
                case.case_id,
                parsed.err()
            );
        }
    }

    #[test]
    fn generic_kit_does_not_special_case_json_select() {
        let spec = public_spec().to_string();
        assert!(
            !spec.contains("json_select"),
            "generic kit must not mention json_select: {spec}"
        );
    }
}
