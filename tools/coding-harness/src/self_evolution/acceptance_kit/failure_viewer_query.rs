//! Acceptance contract for `external.failure_viewer_query`.

use super::PrivateVerificationCase;
use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};

const FIXTURE: &str = r#"{
  "rendered": {
    "component_id": "failure-viewer",
    "component_version": "0.1.2",
    "health": "ready",
    "failure_count": 2,
    "failure_events": [
      {
        "capability_name": "external.alpha",
        "failed_stage": "external_execution",
        "error_category": "timeout",
        "detail_code": "UPSTREAM_TIMEOUT",
        "run_id": "run-alpha",
        "invocation_id": "inv-1",
        "receipt_status": "Failed",
        "receipt_time": "2026-07-15T10:00:00Z"
      },
      {
        "capability_name": "external.coding_task_submit",
        "failed_stage": "external_execution",
        "error_category": "external_configuration_missing",
        "detail_code": "GENERATOR_NOT_CONFIGURED_FOR_PROFILE",
        "run_id": "run-beta",
        "invocation_id": "inv-2",
        "receipt_status": "Failed",
        "receipt_time": "2026-07-16T14:30:00Z"
      }
    ]
  }
}"#;

const SECOND_FIXTURE: &str = r#"{
  "rendered": {
    "component_id": "failure-viewer",
    "component_version": "0.1.3",
    "health": "ready",
    "failure_count": 2,
    "failure_events": [
      {
        "capability_name": "external.gamma",
        "failed_stage": "external_execution",
        "error_category": "connect_failed",
        "detail_code": "UPSTREAM_CONNECT_FAILED",
        "run_id": "run-gamma",
        "invocation_id": "inv-3",
        "receipt_status": "Failed",
        "receipt_time": "2026-07-19T07:00:00Z"
      },
      {
        "capability_name": "external.delta",
        "failed_stage": "external_execution",
        "error_category": "protocol_mismatch",
        "detail_code": "UPSTREAM_PROTOCOL_MISMATCH",
        "run_id": "run-delta",
        "invocation_id": "inv-4",
        "receipt_status": "Failed",
        "receipt_time": "2026-07-20T08:15:00Z"
      }
    ]
  }
}"#;

pub(super) fn private_verification_cases() -> &'static [PrivateVerificationCase] {
    &[
        PrivateVerificationCase {
            case_id: "failure-viewer-query-A",
            input: FIXTURE,
            evaluation_time_utc: "2026-07-18T00:00:00Z",
        },
        PrivateVerificationCase {
            case_id: "failure-viewer-query-B",
            input: SECOND_FIXTURE,
            evaluation_time_utc: "2026-07-21T00:00:00Z",
        },
        PrivateVerificationCase {
            case_id: "failure-viewer-query-empty",
            input: r#"{"rendered":{"component_id":"failure-viewer","component_version":"0.1.2","health":"ready","failure_count":0,"failure_events":[]}}"#,
            evaluation_time_utc: "2026-07-18T00:00:00Z",
        },
    ]
}

pub fn public_spec() -> Value {
    json!({
        "kit_id": "failure-viewer-query-v0",
        "kit_version": "v0",
        "target_profile": "invocable-capability-v0",
        "upstream": {
            "component_id": "failure-viewer",
            "method": "GET",
            "path": "/api/state",
            "discovery": "deployment-harness-component-registry"
        },
        "upstream_schema": {
            "shape": "The upstream value passed to transform is the full /api/state JSON body. Failure events live at upstream[\"rendered\"][\"failure_events\"], NOT at the top level.",
            "rendered_path": "rendered",
            "failure_events_path": "rendered.failure_events",
            "example_input": {
                "rendered": {
                    "component_id": "failure-viewer",
                    "component_version": "0.1.2",
                    "health": "ready",
                    "failure_count": 1,
                    "failure_events": [
                        {
                            "capability_name": "external.example",
                            "failed_stage": "external_execution",
                            "error_category": "timeout",
                            "detail_code": "UPSTREAM_TIMEOUT",
                            "run_id": "run-example",
                            "invocation_id": "inv-example",
                            "receipt_status": "Failed",
                            "receipt_time": "2026-01-01T00:00:00Z"
                        }
                    ]
                }
            }
        },
        "transform_interface": "pub fn transform(upstream: &Value) -> Value",
        "requirements": [
            "Read failure_events from upstream[\"rendered\"][\"failure_events\"] (nested under the \"rendered\" key, not at the top level)",
            "Select the most recent failure_events entry by receipt_time",
            "Return capability_name, failed_stage, error_category, detail_code, run_id, invocation_id, receipt_status, and receipt_time from the selected entry, plus source_component set to the literal string \"failure-viewer\"",
            "Return {\"status\":\"no_failures\",\"source_component\":\"failure-viewer\"} when failure_events is empty or the rendered key is absent"
        ],
        "output_example": {
            "capability_name": "external.example",
            "failed_stage": "external_execution",
            "error_category": "timeout",
            "detail_code": "UPSTREAM_TIMEOUT",
            "run_id": "run-example",
            "invocation_id": "inv-example",
            "receipt_status": "Failed",
            "receipt_time": "2026-01-01T00:00:00Z",
            "source_component": "failure-viewer"
        },
        "prohibited": [
            "No filesystem, network, environment, process, thread, unsafe, or Journal access",
            "No chat-context fallback and no invented failure facts"
        ]
    })
}

pub fn verify(
    _request: &DevelopmentRequest,
    _source: &str,
    input: &str,
    stdout: &str,
) -> Result<(), String> {
    let output: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| "INVOCABLE_PROFILE_OUTPUT_INVALID".to_string())?;
    let result = output
        .get("result")
        .filter(|_| output.get("ok") == Some(&json!(true)))
        .ok_or_else(|| "INVOCABLE_PROFILE_RESULT_MISSING".to_string())?;
    let input: Value =
        serde_json::from_str(input).map_err(|_| "INVOCABLE_PRIVATE_INPUT_INVALID".to_string())?;
    let failures = input
        .pointer("/rendered/failure_events")
        .and_then(Value::as_array)
        .ok_or_else(|| "INVOCABLE_PRIVATE_INPUT_INVALID".to_string())?;
    if failures.is_empty() {
        if result.get("status").and_then(Value::as_str) != Some("no_failures")
            || result.get("source_component").and_then(Value::as_str) != Some("failure-viewer")
            || result.as_object().map(serde_json::Map::len) != Some(2)
        {
            return Err("INVOCABLE_NO_FAILURES_RESULT_MISMATCH".into());
        }
        return Ok(());
    }
    let latest = failures
        .iter()
        .filter_map(|failure| {
            failure
                .get("receipt_time")
                .and_then(Value::as_str)
                .map(|time| (time, failure))
        })
        .max_by_key(|(time, _)| *time)
        .map(|(_, failure)| failure)
        .ok_or_else(|| "INVOCABLE_PRIVATE_INPUT_INVALID".to_string())?;
    for field in [
        "capability_name",
        "failed_stage",
        "error_category",
        "detail_code",
        "run_id",
        "invocation_id",
        "receipt_status",
        "receipt_time",
    ] {
        if result.get(field) != latest.get(field) || !result[field].is_string() {
            return Err(format!("INVOCABLE_RESULT_FIELD_MISMATCH:{field}"));
        }
    }
    if result.get("source_component").and_then(Value::as_str) != Some("failure-viewer")
        || result.as_object().map(serde_json::Map::len) != Some(9)
    {
        return Err("INVOCABLE_RESULT_FIELD_MISMATCH:source_component".into());
    }
    Ok(())
}

pub fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("trusted fixture is valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_accepts_latest_failure_facts() {
        let output = json!({
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
        });
        assert!(verify(&dummy_request(), "", FIXTURE, &output.to_string()).is_ok());
    }

    #[test]
    fn verifier_accepts_bounded_empty_result() {
        let input = private_verification_cases()[2].input;
        let output = json!({
            "ok": true,
            "result": {"status": "no_failures", "source_component": "failure-viewer"}
        });
        assert!(verify(&dummy_request(), "", input, &output.to_string()).is_ok());
    }

    #[test]
    fn verifier_derives_a_different_latest_failure() {
        let output = json!({
            "ok": true,
            "result": {
                "capability_name": "external.delta",
                "failed_stage": "external_execution",
                "error_category": "protocol_mismatch",
                "detail_code": "UPSTREAM_PROTOCOL_MISMATCH",
                "run_id": "run-delta",
                "invocation_id": "inv-4",
                "receipt_status": "Failed",
                "receipt_time": "2026-07-20T08:15:00Z",
                "source_component": "failure-viewer"
            }
        });
        assert!(verify(&dummy_request(), "", SECOND_FIXTURE, &output.to_string()).is_ok());
    }

    #[test]
    fn verifier_rejects_invented_latest_failure() {
        let output = json!({
            "ok": true,
            "result": {
                "capability_name": "external.coding_task_submit",
                "failed_stage": "external_execution",
                "error_category": "timeout",
                "detail_code": "GENERATOR_NOT_CONFIGURED_FOR_PROFILE",
                "run_id": "run-beta",
                "invocation_id": "inv-2",
                "receipt_status": "Failed",
                "receipt_time": "2026-07-16T14:30:00Z",
                "source_component": "failure-viewer"
            }
        });
        let error = verify(&dummy_request(), "", FIXTURE, &output.to_string()).unwrap_err();
        assert_eq!(error, "INVOCABLE_RESULT_FIELD_MISMATCH:error_category");
    }

    fn dummy_request() -> DevelopmentRequest {
        use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
        use agent_core_kernel::domain::{DevelopmentRequestDraft, TargetKind};
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.failure_viewer_query".into(),
        );
        draft.requirements = vec!["query failure viewer".into()];
        draft.required_contracts = vec!["failure-viewer.api-state.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["return latest failure facts".into()];
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
}
