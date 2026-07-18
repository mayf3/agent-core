//! Failure Event Viewer Acceptance Kit: public_spec + private_verifier.
//!
//! Displays model invocation failure events in a read-only page.
//! Does NOT contain any token/metrics fields (rolling_windows, token
//! breakdown, by_model, by_profile, run-1, model-a).

use super::constraint_diagnostic;
use super::shared_verifier_engine::validate_events_applied;
use crate::self_evolution::acceptance_kit::PrivateVerificationCase;
use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};

/// Private verification cases for Failure Event Viewer.
///
/// Each case contains only `model.invocation.failed.v0` events (no token
/// business fields). Evaluation time is frozen for deterministic results.
pub(super) fn private_verification_cases() -> &'static [PrivateVerificationCase] {
    &[
        PrivateVerificationCase {
            case_id: "fev-case-A",
            evaluation_time_utc: "2026-07-18T00:00:00Z",
            input: r#"{"schema_version":"event.observe.v0","next_cursor":3,"has_more":false,"events":[
                {"event_id":"fail-1","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"batch-run","payload":{"model":"gpt-4","provider":"openai","profile":"default","error_category":"rate_limited","latency_ms":5000}},
                {"event_id":"fail-2","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-16T14:30:00Z","run_id":"batch-run","payload":{"model":"claude-3","provider":"anthropic","profile":"production","error_category":"timeout","latency_ms":120000}},
                {"event_id":"fail-3","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-17T09:00:00Z","run_id":"ad-hoc","payload":{"model":"llama-3","provider":"meta","profile":"eval","error_category":"dependency_unavailable","latency_ms":30000}}
            ]}"#,
        },
    ]
}

/// Public specification for the Failure Event Viewer kit.
///
/// This is shown to the model during generation and repair as part of
/// the per-request context.
pub fn public_spec() -> Value {
    json!({
        "kit_id": "failure-event-viewer-v0",
        "kit_version": "v0",
        "target_profile": "hook-consumer-service-v0",
        "description": "Failure Event Viewer — display model invocation failure events in a readable table.",

        "input_contract": {
            "contract_id": "event.observe.v0",
            "event_types": [
                "model.invocation.failed.v0"
            ],
            "allowed_fields": {
                "event_id": "string — unique event identifier",
                "event_kind": "string — must be model.invocation.failed.v0",
                "occurred_at": "RFC 3339 timestamp — when the failure occurred",
                "run_id": "string — the run identifier",
                "payload.model": "string — the model identifier that failed",
                "payload.provider": "string — the LLM provider",
                "payload.profile": "string — the profile/owner identifier",
                "payload.error_category": "string — stable error category (rate_limited, timeout, dependency_unavailable, content_filter, internal_error, etc.)",
                "payload.latency_ms": "integer — latency in milliseconds before the failure"
            },
            "missing_field_handling": "Absent fields are shown as 'unknown' in the display.",
            "time_format": "RFC 3339 in UTC"
        },

        "output_json_schema": {
            "type": "object",
            "required": ["rendered"],
            "properties": {
                "rendered": {
                    "type": "object",
                    "description": "Application state visible to the test harness",
                    "properties": {
                        "failure_events": {
                            "type": "array",
                            "description": "List of failure events with key details"
                        },
                        "failure_count": {
                            "type": "integer",
                            "description": "Total count of failure events"
                        },
                        "telemetry_unavailable": {
                            "type": "boolean",
                            "description": "Runtime telemetry unavailable flag"
                        },
                        "last_observed_cursor": {
                            "type": "integer",
                            "description": "Last processed event cursor"
                        },
                        "projection_lag": {
                            "type": "string",
                            "description": "Projection lag status"
                        },
                        "component_version": {
                            "type": "string",
                            "description": "Component version"
                        },
                        "health": {
                            "type": "string",
                            "description": "Health status"
                        }
                    }
                }
            }
        },

        "html_contract": {
            "required_display": [
                "Failure events in a table or list",
                "Event ID or identifier",
                "Run ID showing which run failed",
                "Model that failed",
                "Error category for each failure",
                "Timestamp of when the failure occurred",
                "Failure count or total",
                "Runtime metadata (component_id, component_version, health, projection_lag, telemetry_unavailable)"
            ],
            "behavior": "read_only",
            "prohibited": [
                "No mutation or modification requests",
                "No scripts or external assets"
            ],
            "style_requirement": "The page must be readable. All event-derived text must be HTML-escaped."
        },

        "examples": [
            {
                "description": "Example with one model.invocation.failed.v0 event",
                "input": {
                    "events": [
                        {"event_id": "fail-1", "event_kind": "model.invocation.failed.v0", "occurred_at": "2026-07-15T10:00:00Z", "run_id": "batch-run", "payload": {"model": "gpt-4", "provider": "openai", "profile": "default", "error_category": "rate_limited", "latency_ms": 5000}}
                    ]
                },
                "output_hint": "The rendered output should include failure_events array with one entry showing event_id=fail-1, run_id=batch-run, model=gpt-4, error_category=rate_limited, occurred_at=2026-07-15T10:00:00Z. These are illustrative — your implementation must compute from real events."
            }
        ],

        "notes": {
            "no_token_fields": "This kit does NOT require token-specific metrics (rolling window totals, per-model/per-profile dimensions, or token breakdowns). Only failure event display fields are needed.",
            "empty_result": "When there are no failure events, show an empty state (e.g., 'No failures recorded') with zeroed failure_count."
        }
    })
}

/// Private verifier for Failure Event Viewer.
///
/// Validates that the generated module:
/// 1. Passes the profile contract
/// 2. Renders failure event fields without token-specific contamination
/// 3. Follows the expected output shape
pub fn verify(
    _request: &DevelopmentRequest,
    _source: &str,
    input: &str,
    stdout: &str,
) -> Result<(), String> {
    // Events-applied check (shared logic — count comes from actual input)
    validate_events_applied(input, stdout)?;

    // Profile contract check (fields other than events_applied)
    let output: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| "PROFILE_CONTRACT_OUTPUT_INVALID".to_string())?;
    let mut missing = Vec::new();
    for (field, expected) in [
        ("ok", json!(true)),
        ("schema_version", json!("hook-consumer-service-contract-v0")),
        ("html_nonempty", json!(true)),
        ("html_safe", json!(true)),
        ("html_runtime_metadata", json!(true)),
    ] {
        if output.get(field) != Some(&expected) {
            missing.push(field);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "PROFILE_CONTRACT_TEST_FAILED missing={}\nHTML_RUNTIME_METADATA_CONTRACT: render_html must visibly include the supplied runtime component_id, component_version, health, projection_lag, and telemetry_unavailable values.\nPROFILE_OUTPUT:\n{}",
            missing.join(","),
            truncate(stdout),
        ));
    }

    // Render contract: failure event fields + generic runtime metadata
    let rendered = output.get("rendered").ok_or_else(|| {
        constraint_diagnostic(
            "json.rendered.required",
            "$.rendered",
            "required object",
            "missing",
        )
    })?;
    let rendered_text = serde_json::to_string(rendered)
        .map_err(|_| "RENDER_CONTRACT_OUTPUT_INVALID".to_string())?
        .to_lowercase();

    let mut render_missing = Vec::new();

    // Generic runtime metadata
    for (label, aliases) in [
        ("telemetry_unavailable", &["telemetry_unavailable"][..]),
        ("last_observed_cursor", &["last_observed_cursor"][..]),
        ("projection_lag", &["projection_lag"][..]),
        ("component_version", &["component_version"][..]),
        ("health", &["health"][..]),
    ] {
        if !aliases.iter().any(|marker| rendered_text.contains(marker)) {
            render_missing.push(label.to_string());
        }
    }

    // Failure event specific: check that the rendered output contains
    // failure-related terms (not token-specific terms).
    let failure_terms = ["failure", "fail_count", "error", "error_category"];
    if !failure_terms
        .iter()
        .any(|term| rendered_text.contains(term))
    {
        render_missing.push("failure-or-error".into());
    }

    // Must NOT contain token dashboard contamination terms
    let token_terms = [
        "rolling_windows",
        "by_model",
        "by_profile",
        "input_tokens",
        "cached_tokens",
        "output_tokens",
        "reasoning_tokens",
    ];
    for term in &token_terms {
        if rendered_text.contains(term) {
            return Err(format!(
                "CROSS_KIT_CONTAMINATION_FAILED: rendered output contains token-specific field '{term}' which is not part of the Failure Event Viewer kit"
            ));
        }
    }

    if render_missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED missing={}\n{}",
            render_missing.join(","),
            "RENDER_CONTRACT: render_json must return a Value containing all relevant failure event state. render_html must return a readable HTML page that visibly includes the supplied runtime metadata (component_id, component_version, health, projection_lag, telemetry_unavailable). Include failure event details (error category, model, run, timestamp) in the output."
        ))
    }
}

fn truncate(value: &str) -> String {
    let mut end = value.len().min(16 * 1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_viewer_public_spec_is_valid_json() {
        let spec = public_spec();
        assert!(spec.is_object());
        assert_eq!(spec["kit_id"], "failure-event-viewer-v0");
        assert!(spec.get("output_json_schema").is_some());
        assert!(spec.get("html_contract").is_some());
        assert!(spec.get("examples").is_some());
    }

    #[test]
    fn failure_viewer_spec_contains_no_token_terms() {
        let spec = public_spec();
        let schema = serde_json::to_string(&spec["output_json_schema"])
            .unwrap()
            .to_lowercase();
        let html = serde_json::to_string(&spec["html_contract"])
            .unwrap()
            .to_lowercase();
        for forbidden in &[
            "rolling_windows",
            "by_model",
            "by_profile",
            "run-1",
            "model-a",
        ] {
            assert!(
                !schema.contains(forbidden),
                "schema must not contain '{forbidden}'"
            );
            assert!(
                !html.contains(forbidden),
                "html must not contain '{forbidden}'"
            );
        }
    }

    #[test]
    fn verify_passes_valid_failure_output() {
        let input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
        let output = json!({
            "ok": true,
            "schema_version": "hook-consumer-service-contract-v0",
            "events_applied": 3,
            "html_nonempty": true,
            "html_safe": true,
            "html_runtime_metadata": true,
            "rendered": {
                "failure_events": [
                    {"event_id": "fail-1", "run_id": "run-x", "error_category": "timeout"}
                ],
                "failure_count": 1,
                "telemetry_unavailable": false,
                "last_observed_cursor": 3,
                "projection_lag": "caught_up",
                "component_version": "0.1.0",
                "health": "ready"
            }
        });
        assert!(verify(&dummy_request(), "", input, &output.to_string()).is_ok());
    }

    #[test]
    fn verify_rejects_token_contamination() {
        let input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
        let output = json!({
            "ok": true,
            "schema_version": "hook-consumer-service-contract-v0",
            "events_applied": 3,
            "html_nonempty": true,
            "html_safe": true,
            "html_runtime_metadata": true,
            "rendered": {
                "failure_events": [{"event_id": "f1"}],
                "rolling_windows": {"1_day": {"calls": 2}},
                "telemetry_unavailable": false,
                "last_observed_cursor": 3,
                "projection_lag": "caught_up",
                "component_version": "0.1.0",
                "health": "ready"
            }
        });
        let result = verify(&dummy_request(), "", input, &output.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("CROSS_KIT_CONTAMINATION"));
    }

    fn dummy_request() -> DevelopmentRequest {
        use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
        use agent_core_kernel::domain::{DevelopmentRequestDraft, TargetKind};
        let mut draft =
            DevelopmentRequestDraft::new(TargetKind::HookConsumerService, "failure-viewer".into());
        draft.requirements = vec!["display failure events".into()];
        draft.required_contracts = vec!["event.observe.v0".into()];
        draft.requested_permissions = vec!["journal.observe".into()];
        draft.acceptance_criteria = vec!["failure page".into()];
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
