//! Shared verification engine used by all Acceptance Kits.
//!
//! Changing this file affects the bundle digest of every kit that
//! depends on it (via build.rs per-kit bundle computation).

use serde_json::Value;

/// Format a structured constraint diagnostic for model-visible feedback.
///
/// This is the canonical form for communicating acceptance constraint
/// violations back to the model during repair rounds. The format is:
///
/// ```text
/// ACCEPTANCE_CONSTRAINT: <constraint_id>
/// PATH: <path>
/// EXPECTED: <expected>
/// ACTUAL: <actual>
/// ```
pub fn constraint_diagnostic(
    constraint_id: &str,
    path: &str,
    expected: &str,
    actual: &str,
) -> String {
    format!(
        "ACCEPTANCE_CONSTRAINT: {constraint_id}\nPATH: {path}\nEXPECTED: {expected}\nACTUAL: {actual}"
    )
}

/// Truncate diagnostics to a safe maximum length, preserving UTF-8 boundaries.
pub fn truncate_diagnostics(value: &str) -> String {
    let max_len = 16 * 1024;
    let mut end = value.len().min(max_len);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// Sanitize model diagnostics by replacing the generator root and candidate
/// id with safe placeholders, preventing host path disclosure.
pub fn sanitize_model_diagnostics(
    diagnostics: &str,
    base: &std::path::Path,
    candidate_id: &str,
) -> String {
    let root_repr = "<generator-root>";
    let id_repr = "<candidate-id>";
    let sanitized = diagnostics
        .replace(base.to_str().unwrap_or(""), root_repr)
        .replace(candidate_id, id_repr);
    sanitized
}

/// Validate that `events_applied` in `output` matches the number of events
/// in the input JSON.
///
/// # Errors
///
/// Returns an error if:
/// - `input` is not valid JSON.
/// - `input` lacks an `events` field.
/// - `events` is not a JSON array.
/// - `output` is not valid JSON.
/// - `output` lacks an `events_applied` field.
/// - `events_applied` is not a positive integer.
/// - The count does not match.
pub fn validate_events_applied(input: &str, output: &str) -> Result<(), String> {
    let input_value: Value = serde_json::from_str(input)
        .map_err(|_| constraint_diagnostic("json.parse", "$", "valid JSON", "parse failure"))?;
    let events = input_value.get("events").ok_or_else(|| {
        constraint_diagnostic(
            "json.events.required",
            "$.events",
            "required array",
            "missing",
        )
    })?;
    let event_count = events
        .as_array()
        .ok_or_else(|| constraint_diagnostic("json.events.type", "$.events", "array", "non-array"))?
        .len();

    let output_value: Value = serde_json::from_str(output)
        .map_err(|_| constraint_diagnostic("json.parse", "$", "valid JSON", "parse failure"))?;
    let applied = output_value.get("events_applied").ok_or_else(|| {
        constraint_diagnostic(
            "json.events_applied.required",
            "$.events_applied",
            "required number",
            "missing",
        )
    })?;
    let applied_count = applied.as_u64().ok_or_else(|| {
        constraint_diagnostic(
            "json.events_applied.type",
            "$.events_applied",
            "positive integer",
            "non-integer",
        )
    })? as usize;

    if event_count != applied_count {
        return Err(format!(
            "EVENTS_APPLIED_MISMATCH: input has {event_count} events but output reports {applied_count} events_applied"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_events_applied_two_passes() {
        let input = r#"{"events":[{"id":1},{"id":2}]}"#;
        let output = r#"{"events_applied":2}"#;
        assert!(validate_events_applied(input, output).is_ok());
    }

    #[test]
    fn two_events_applied_three_fails() {
        let input = r#"{"events":[{"id":1},{"id":2}]}"#;
        let output = r#"{"events_applied":3}"#;
        let err = validate_events_applied(input, output).unwrap_err();
        assert!(err.contains("EVENTS_APPLIED_MISMATCH"));
        assert!(err.contains("2 events"));
        assert!(err.contains("3 events_applied"));
    }

    #[test]
    fn three_events_applied_three_passes() {
        let input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
        let output = r#"{"events_applied":3}"#;
        assert!(validate_events_applied(input, output).is_ok());
    }

    #[test]
    fn missing_events_array_fails() {
        let input = r#"{"no_events":true}"#;
        let output = r#"{"events_applied":0}"#;
        let err = validate_events_applied(input, output).unwrap_err();
        assert!(err.contains("json.events.required"));
    }

    #[test]
    fn events_field_not_array_fails() {
        let input = r#"{"events":"not_an_array"}"#;
        let output = r#"{"events_applied":0}"#;
        let err = validate_events_applied(input, output).unwrap_err();
        assert!(err.contains("json.events.type"));
    }

    #[test]
    fn output_missing_events_applied_fails() {
        let input = r#"{"events":[{"id":1}]}"#;
        let output = r#"{"no_applied":true}"#;
        let err = validate_events_applied(input, output).unwrap_err();
        assert!(err.contains("json.events_applied.required"));
    }

    #[test]
    fn events_applied_not_integer_fails() {
        let input = r#"{"events":[{"id":1}]}"#;
        let output = r#"{"events_applied":"two"}"#;
        let err = validate_events_applied(input, output).unwrap_err();
        assert!(err.contains("json.events_applied.type"));
    }

    #[test]
    fn invalid_input_json_fails() {
        let input = r#"not json"#;
        let output = r#"{"events_applied":0}"#;
        let err = validate_events_applied(input, output).unwrap_err();
        assert!(err.contains("json.parse"));
    }

    #[test]
    fn invalid_output_json_fails() {
        let input = r#"{"events":[{"id":1}]}"#;
        let output = r#"not json"#;
        let err = validate_events_applied(input, output).unwrap_err();
        assert!(err.contains("json.parse"));
    }

    /// Behavioral proof that both kits use the shared validate_events_applied
    /// function instead of a hardcoded magic number. Each verifier is called
    /// with input containing 2 events — if it still hardcoded `events_applied=3`
    /// the 2→2 match would fail when the output reports 2.
    ///
    /// These tests live here (not in the kit modules) so they import the
    /// verify functions from the sibling modules, proving end-to-end that
    /// the no-magic-number contract is satisfied.
    mod magic_number_eradication {
        use crate::self_evolution::acceptance_kit::{failure_event_viewer, token_dashboard};
        use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
        use agent_core_kernel::domain::{DevelopmentRequest, DevelopmentRequestDraft, TargetKind};

        fn request(name: &str) -> DevelopmentRequest {
            let mut draft =
                DevelopmentRequestDraft::new(TargetKind::HookConsumerService, name.into());
            draft.requirements = vec!["test".into()];
            draft.required_contracts = vec!["event.observe.v0".into()];
            draft.requested_permissions = vec!["journal.observe".into()];
            draft.acceptance_criteria = vec!["test".into()];
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

        fn valid_output(events_applied: u64) -> String {
            use serde_json::json;
            json!({
                "ok": true,
                "schema_version": "hook-consumer-service-contract-v0",
                "events_applied": events_applied,
                "html_nonempty": true,
                "html_safe": true,
                "html_runtime_metadata": true,
                "html_telemetry_metrics": true,
                "html_average_latency": true,
                "rendered": {
                    "rolling_windows": {
                        "1_day": {
                            "calls": 2,
                            "avg_latency": 25.0,
                            "failures": 1,
                            "unavailable": 1
                        },
                        "7_day": {
                            "calls": 2,
                            "avg_latency": 25.0,
                            "failures": 1,
                            "unavailable": 1
                        },
                        "30_day": {
                            "calls": 2,
                            "avg_latency": 25.0,
                            "failures": 1,
                            "unavailable": 1
                        }
                    },
                    "run-1": true,
                    "model-a": true,
                    "default": true,
                    "2026-07-15": true,
                    "input": true,
                    "cached": true,
                    "output": true,
                    "reasoning": true,
                    "latency": true,
                    "failure": true,
                    "unavailable": 1,
                    "telemetry_unavailable": false,
                    "last_observed_cursor": events_applied,
                    "projection_lag": "caught_up",
                    "component_version": "0.1.0",
                    "health": "ready"
                }
            })
            .to_string()
        }

        fn valid_fev_output(events_applied: u64) -> String {
            use serde_json::json;
            json!({
                "ok": true,
                "schema_version": "hook-consumer-service-contract-v0",
                "events_applied": events_applied,
                "html_nonempty": true,
                "html_safe": true,
                "html_runtime_metadata": true,
                "rendered": {
                    "failure_events": [
                        {"event_id": "f1", "run_id": "r1", "error_category": "timeout"}
                    ],
                    "failure_count": 1,
                    "telemetry_unavailable": false,
                    "last_observed_cursor": events_applied,
                    "projection_lag": "caught_up",
                    "component_version": "0.1.0",
                    "health": "ready"
                }
            })
            .to_string()
        }

        fn valid_source() -> &'static str {
            r#"pub fn initial_state() -> serde_json::Value { serde_json::json!({}) }
pub fn apply_event(state: &mut serde_json::Value, event: &serde_json::Value) { let _ = (state, event); }
pub fn render_json(state: &serde_json::Value, runtime: &serde_json::Value) -> serde_json::Value { serde_json::json!({"state":state,"runtime":runtime}) }
pub fn render_html(state: &serde_json::Value, runtime: &serde_json::Value) -> String { let _ = (state, runtime); String::new() }"#
        }

        #[test]
        fn token_dashboard_accepts_2_events_applied_2() {
            let input = r#"{"events":[{"id":1},{"id":2}]}"#;
            let req = request("token-dashboard");
            assert!(
                token_dashboard::verify(&req, valid_source(), input, &valid_output(2)).is_ok(),
                "Token Dashboard must accept 2 events → events_applied=2"
            );
        }

        #[test]
        fn token_dashboard_rejects_2_events_applied_3() {
            let input = r#"{"events":[{"id":1},{"id":2}]}"#;
            let req = request("token-dashboard");
            let err =
                token_dashboard::verify(&req, valid_source(), input, &valid_output(3)).unwrap_err();
            assert!(
                err.contains("EVENTS_APPLIED_MISMATCH"),
                "Token Dashboard must reject 2 events → events_applied=3: {err}"
            );
        }

        #[test]
        fn failure_viewer_accepts_2_events_applied_2() {
            let input = r#"{"events":[{"id":1},{"id":2}]}"#;
            let req = request("failure-viewer");
            assert!(
                failure_event_viewer::verify(&req, valid_source(), input, &valid_fev_output(2))
                    .is_ok(),
                "Failure Event Viewer must accept 2 events → events_applied=2"
            );
        }

        #[test]
        fn failure_viewer_rejects_2_events_applied_3() {
            let input = r#"{"events":[{"id":1},{"id":2}]}"#;
            let req = request("failure-viewer");
            let err =
                failure_event_viewer::verify(&req, valid_source(), input, &valid_fev_output(3))
                    .unwrap_err();
            assert!(
                err.contains("EVENTS_APPLIED_MISMATCH"),
                "Failure Event Viewer must reject 2 events → events_applied=3: {err}"
            );
        }

        #[test]
        fn token_dashboard_accepts_3_events_applied_3() {
            let input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
            let req = request("token-dashboard");
            assert!(
                token_dashboard::verify(&req, valid_source(), input, &valid_output(3)).is_ok(),
                "Token Dashboard must accept 3 events → events_applied=3"
            );
        }

        #[test]
        fn failure_viewer_accepts_3_events_applied_3() {
            let input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
            let req = request("failure-viewer");
            assert!(
                failure_event_viewer::verify(&req, valid_source(), input, &valid_fev_output(3))
                    .is_ok(),
                "Failure Event Viewer must accept 3 events → events_applied=3"
            );
        }
    }
}
