//! Token Dashboard Acceptance Kit: public_spec + private_verifier.
//!
//! The public specification describes the output JSON schema, aggregation
//! semantics, and HTML contract that the model must follow. The private
//! verifier checks that the generated module produces correct output
//! derived from actual event computation, not hardcoded example values.

use super::constraint_diagnostic;
use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};

/// Public specification for the Token Dashboard kit.
///
/// This is shown to the model during generation and repair as part of
/// the per-request context (not the SYSTEM_PROMPT).
pub fn public_spec() -> Value {
    json!({
        "kit_id": "token-dashboard-v0",
        "kit_version": "v0",
        "target_profile": "hook-consumer-service-v0",
        "description": "Token Dashboard — visualize model token usage, call counts, latency, failures, and availability across 1/7/30-day rolling windows, grouped by model, profile, and run.",

        "input_contract": {
            "contract_id": "event.observe.v0",
            "event_types": [
                "model.invocation.completed.v0",
                "model.invocation.failed.v0"
            ],
            "allowed_fields": {
                "event_id": "string — unique event identifier",
                "event_kind": "string — the kind of invocation event",
                "occurred_at": "RFC 3339 timestamp — when the event occurred",
                "run_id": "string — the run identifier, directly on the event envelope (not nested in payload)",
                "payload.profile": "string — the user/profile identifier for this invocation, may be absent",
                "payload.provider": "string — the LLM provider used",
                "payload.model": "string — the model identifier",
                "payload.latency_ms": "integer — invocation latency in milliseconds, present for completed and failed events",
                "payload.input_tokens": "nullable integer — input token count, may be null or absent",
                "payload.cached_input_tokens": "nullable integer — cached input token count, may be null or absent",
                "payload.output_tokens": "nullable integer — output token count, may be null or absent",
                "payload.reasoning_tokens": "nullable integer — reasoning token count, may be null or absent",
                "payload.total_tokens": "nullable integer — total token count, may be null or absent",
                "payload.error_category": "string — error category for failed invocations"
            },
            "missing_field_handling": "When a token field is null or absent, count it as unavailable (positive unavailable counter) rather than treating it as zero. Use a separate positive counter for unavailable token fields.",
            "time_format": "RFC 3339 in UTC (e.g. 2026-07-15T10:00:00Z)",
            "token_numeric_type": "u64 (non-negative integer)"
        },

        "output_json_schema": {
            "type": "object",
            "required": ["rolling_windows"],
            "properties": {
                "rolling_windows": {
                    "type": "object",
                    "description": "Container for 1-day, 7-day, and 30-day rolling windows",
                    "properties": {
                        "1_day": {"$ref": "#/definitions/window_set"},
                        "7_day": {"$ref": "#/definitions/window_set"},
                        "30_day": {"$ref": "#/definitions/window_set"}
                    }
                }
            },
            "definitions": {
                "window_set": {
                    "type": "object",
                    "description": "Aggregated metrics for one window size",
                    "properties": {
                        "overall": {"$ref": "#/definitions/window_metrics"},
                        "by_model": {
                            "type": "object",
                            "description": "Metrics keyed by model identifier",
                            "additionalProperties": {"$ref": "#/definitions/window_metrics"}
                        },
                        "by_profile": {
                            "type": "object",
                            "description": "Metrics keyed by profile identifier",
                            "additionalProperties": {"$ref": "#/definitions/window_metrics"}
                        }
                    }
                },
                "window_metrics": {
                    "type": "object",
                    "properties": {
                        "calls": {"type": "integer", "description": "Total invocations (completed + failed) in the window"},
                        "input_tokens": {"type": "integer", "description": "Total input tokens"},
                        "cached_tokens": {"type": "integer", "description": "Total cached input tokens"},
                        "output_tokens": {"type": "integer", "description": "Total output tokens"},
                        "reasoning_tokens": {"type": "integer", "description": "Total reasoning tokens"},
                        "total_tokens": {"type": "integer", "description": "Total tokens (including cached)"},
                        "avg_latency_ms": {"type": "number", "description": "Average latency across all invocations with latency_ms present"},
                        "failures": {"type": "integer", "description": "Count of failed invocations"},
                        "unavailable": {"type": "integer", "description": "Count of missing token fields across invocations"}
                    }
                }
            }
        },

        "aggregation_semantics": {
            "window_boundary": "1_day covers today (based on runtime today_utc), 7_day covers today + 6 prior days, 30_day covers today + 29 prior days. Use within_days helper with event_date.",
            "date_attribution": "Events are attributed to the date in their occurred_at field, not the processing date.",
            "success_and_failure_counting": "Both completed and failed invocations count toward calls and avg_latency_ms when latency_ms is present. Failures are additionally counted in the failures field.",
            "average_latency_computation": "Sum of all latency_ms values divided by count of invocations with non-null latency_ms. Use integer arithmetic; display as decimal if needed.",
            "missing_token_handling": "Any null or absent token field increments the unavailable counter for that invocation. Do NOT estimate missing values as zero.",
            "profile_missing_grouping": "When profile is absent from the event payload, use the string 'default' as the profile identifier.",
            "cached_tokens_in_total": "cached_tokens IS included in total_tokens (total_tokens = input_tokens + cached_tokens + output_tokens + reasoning_tokens).",
            "total_token_aggregation": "total_tokens is aggregated from per-invocation total_tokens values, not recomputed from sub-totals."
        },

        "html_contract": {
            "required_display": [
                "Date of the data",
                "Run identifiers",
                "Model identifiers",
                "Profile identifiers",
                "Token breakdown (input, cached, output, reasoning, total)",
                "Call count and average latency per window and dimension",
                "Failure counts",
                "Unavailable token counters"
            ],
            "behavior": "read_only",
            "prohibited": [
                "No mutation or modification requests",
                "No scripts or external assets",
                "No forms that would POST data"
            ],
            "style_requirement": "The page must be readable. Visual style is not prescribed — any plain HTML table or list is acceptable. All event-derived text must be HTML-escaped."
        },

        "examples": [
            {
                "description": "Example input with two completed events and one failed event",
                "input": {
                    "events": [
                        {"event_id": "e1", "event_kind": "model.invocation.completed.v0", "occurred_at": "2026-07-15T10:00:00Z", "run_id": "run-1", "payload": {"profile": "default", "provider": "test", "model": "model-a", "latency_ms": 20, "input_tokens": 10, "cached_input_tokens": 2, "output_tokens": 5, "reasoning_tokens": 1, "total_tokens": 16}},
                        {"event_id": "e2", "event_kind": "model.invocation.completed.v0", "occurred_at": "2026-07-15T11:00:00Z", "run_id": "run-2", "payload": {"profile": "analysis", "provider": "test", "model": "model-b", "latency_ms": 30, "input_tokens": 20, "cached_input_tokens": null, "output_tokens": 8, "reasoning_tokens": 2, "total_tokens": 30}},
                        {"event_id": "e3", "event_kind": "model.invocation.failed.v0", "occurred_at": "2026-07-15T12:00:00Z", "run_id": "run-1", "payload": {"profile": "default", "provider": "test", "model": "model-a", "latency_ms": null, "error_category": "rate_limited"}}
                    ]
                },
                "output_hint": "The 1_day overall window would have calls=3 (two completed + one failed), avg_latency_ms=25 (50ms from e1+e2, e3 null latency excluded), input_tokens=30, failures=1, unavailable>=1 (e2 has null cached_input_tokens). These are illustrative — your implementation must compute values from real events, not hardcode example values."
            }
        ],

        "notes": {
            "field_name_stability": "All field names in the output JSON schema are stable and required. Do not rename or omit fields.",
            "map_key_rules": "Map keys for by_model and by_profile are the actual model/profile identifiers from the events. Use 'unknown' for missing model values. Use 'default' for missing profile values.",
            "sorting_and_determinism": "Output map keys should be sorted lexicographically for deterministic JSON output.",
            "empty_result": "When there are no events, each rolling window should still be present with zeroed metrics (calls=0, all token totals=0, avg_latency_ms=0, failures=0, unavailable=0) and empty by_model/by_profile maps."
        }
    })
}

/// Private verifier for Token Dashboard.
///
/// Validates that the generated module:
/// 1. Passes the profile contract (hook-consumer-service-contract-v0)
/// 2. Renders the required telemetry fields (runs, models, profiles, windows)
/// 3. Computes values from input events, not hardcoded example values
/// 4. Follows source-level policies (no within_days in apply_event, no today_utc)
pub fn verify(
    _request: &DevelopmentRequest,
    source: &str,
    stdout: &str,
) -> Result<(), String> {
    let mut failures = Vec::new();

    // Profile contract check
    if let Err(error) = verify_profile_contract(stdout) {
        failures.push(error);
    }

    // Request contract check (telemetry-specific fields)
    if let Err(error) = verify_request_contract(stdout) {
        failures.push(error);
    }

    // Source policy check
    if let Err(error) = verify_source_policy(source) {
        failures.push(error);
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "COMBINED_CONTRACT_PROBE_FAILED\n{}",
            failures.join("\n---\n")
        ))
    }
}

/// Validate the profile contract output (applies to every hook consumer).
fn verify_profile_contract(stdout: &str) -> Result<(), String> {
    let output: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| "PROFILE_CONTRACT_OUTPUT_INVALID".to_string())?;
    let mut missing = Vec::new();
    for (field, expected) in [
        ("ok", json!(true)),
        ("schema_version", json!("hook-consumer-service-contract-v0")),
        ("events_applied", json!(3)),
        ("html_nonempty", json!(true)),
        ("html_safe", json!(true)),
        ("html_runtime_metadata", json!(true)),
    ] {
        if output.get(field) != Some(&expected) {
            missing.push(field);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "PROFILE_CONTRACT_TEST_FAILED missing={}\nHTML_RUNTIME_METADATA_CONTRACT: render_html must visibly include the supplied runtime component_id, component_version, health, projection_lag, and telemetry_unavailable values.\nPROFILE_OUTPUT:\n{}",
            missing.join(","),
            truncate(stdout),
        ))
    }
}

/// Validate the rendered output for telemetry-specific contract fields.
fn verify_request_contract(stdout: &str) -> Result<(), String> {
    let output: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| constraint_diagnostic(
            "json.parse",
            "$",
            "valid JSON",
            "parse failure",
        ))?;
    let rendered = output
        .get("rendered")
        .ok_or_else(|| constraint_diagnostic(
            "json.rendered.required",
            "$.rendered",
            "required object",
            "missing",
        ))?;
    let rendered_text = serde_json::to_string(rendered)
        .map_err(|_| "REQUEST_CONTRACT_RENDERED_INVALID".to_string())?
        .to_lowercase();

    let mut missing = Vec::new();

    // Generic runtime metadata
    for (label, aliases) in [
        ("telemetry_unavailable", &["telemetry_unavailable"][..]),
        ("last_observed_cursor", &["last_observed_cursor"][..]),
        ("projection_lag", &["projection_lag"][..]),
        ("component_version", &["component_version"][..]),
        ("health", &["health"][..]),
    ] {
        if !aliases.iter().any(|marker| rendered_text.contains(marker)) {
            missing.push(label.to_string());
        }
    }

    // Token Dashboard specific fields
    for (label, aliases) in [
        ("run-1", &["run-1"][..]),
        ("model-a", &["model-a"][..]),
        ("default", &["default"][..]),
        ("2026-07-15", &["2026-07-15"][..]),
        ("input", &["input"][..]),
        ("cached", &["cached"][..]),
        ("output", &["output"][..]),
        ("reasoning", &["reasoning"][..]),
        ("latency", &["latency"][..]),
        ("failure", &["failure", "fail_count", "failures"][..]),
        ("unavailable", &["unavailable"][..]),
    ] {
        if !aliases.iter().any(|marker| rendered_text.contains(marker)) {
            missing.push(label.to_string());
        }
    }

    if !has_positive_counter(rendered, &["unavailable"]) {
        missing.push("positive-unavailable-counter".into());
    }
    if !has_positive_counter(rendered, &["failure", "fail_count", "failures"]) {
        missing.push("positive-failure-counter".into());
    }

    // Rolling windows validation
    for days in [1, 7, 30] {
        if !has_window_key(rendered, days) {
            missing.push(format!("{days}-day-window"));
        }
        if !has_positive_overall_window(rendered, days) {
            missing.push(format!("positive-overall-{days}-day-window"));
        }
        if !requested_overall_window_satisfies(rendered, days, |window| {
            counter_equals(window, &["calls", "call_count", "invocations"], 2.0)
        }) {
            missing.push(format!("overall-{days}-day-call-count=2"));
        }
        if !requested_overall_window_satisfies(rendered, days, |window| {
            counter_equals(window, &["avg_latency", "average_latency", "latency_avg"], 25.0)
        }) {
            missing.push(format!("overall-{days}-day-average-latency=25"));
        }
        if !requested_overall_window_satisfies(rendered, days, |window| {
            has_positive_counter(window, &["unavailable"])
        }) {
            missing.push(format!("positive-overall-{days}-day-unavailable"));
        }
        if !requested_overall_window_satisfies(rendered, days, |window| {
            has_positive_counter(window, &["failure", "fail_count", "failures"])
        }) {
            missing.push(format!("positive-overall-{days}-day-failure"));
        }
    }

    if output.get("html_telemetry_metrics").and_then(Value::as_bool) != Some(true) {
        missing.push("html-telemetry-metrics".into());
    }
    if output.get("html_average_latency").and_then(Value::as_bool) != Some(true) {
        missing.push("html-average-latency".into());
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED missing={}\nPATH_CONTRACT: run dimension comes from top-level event.run_id; model and profile dimensions come from event.payload.model and event.payload.profile.\nWINDOW_CONTRACT: expose overall/global/summary/total windows, a top-level rolling_windows object, or top-level windows whose distinct 1_day, 7_day, and 30_day objects each contain total/overall/summary/global. Each overall window must include calls=2, avg_latency_ms or latency_avg=25, failures=1, and a positive unavailable counter.",
            missing.join(","),
        ))
    }
}

/// Validate that the generated Rust source follows the Token Dashboard policy.
fn verify_source_policy(source: &str) -> Result<(), String> {
    let syntax = syn::parse_file(source)
        .map_err(|_| "REQUEST_SOURCE_CONTRACT_INVALID_RUST".to_string())?;
    let apply = syntax.items.iter().find_map(|item| match item {
        syn::Item::Fn(function) if function.sig.ident == "apply_event" => {
            Some(syn::Item::Fn(function.clone()))
        }
        _ => None,
    });
    let Some(apply) = apply else {
        return Err("REQUEST_SOURCE_CONTRACT_APPLY_EVENT_MISSING".into());
    };
    let apply_source = prettyplease::unparse(&syn::File {
        shebang: None,
        attrs: Vec::new(),
        items: vec![apply],
    });
    if apply_source.contains("within_days(") {
        return Err(
            "REQUEST_SOURCE_CONTRACT_FAILED rolling windows must be derived in render_json/render_html from daily aggregates and runtime today_utc, not frozen in apply_event"
                .into(),
        );
    }
    if source.contains("today_utc()") {
        return Err(
            "REQUEST_SOURCE_CONTRACT_FAILED rolling windows must use runtime.today_utc during render, not the host-clock today_utc() helper"
                .into(),
        );
    }
    Ok(())
}

// ---- Helper functions (moved from contract.rs) ----

fn truncate(value: &str) -> String {
    let mut end = value.len().min(16 * 1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn has_positive_counter(value: &Value, names: &[&str]) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            (names.iter().any(|name| key.to_lowercase().contains(name))
                && contains_positive_number(value))
                || has_positive_counter(value, names)
        }),
        Value::Array(items) => items.iter().any(|item| has_positive_counter(item, names)),
        _ => false,
    }
}

fn contains_positive_number(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_u64().is_some_and(|number| number > 0),
        Value::Object(map) => map.values().any(contains_positive_number),
        Value::Array(items) => items.iter().any(contains_positive_number),
        _ => false,
    }
}

fn has_window_key(value: &Value, days: u64) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| window_key_matches(key, days) || has_window_key(value, days)),
        Value::Array(items) => items.iter().any(|item| has_window_key(item, days)),
        _ => false,
    }
}

fn has_positive_overall_window(value: &Value, days: u64) -> bool {
    requested_overall_window_satisfies(value, days, contains_positive_number)
}

fn requested_overall_window_satisfies<F>(value: &Value, days: u64, check: F) -> bool
where
    F: Fn(&Value) -> bool + Copy,
{
    let direct_window_set = value.as_object().is_some_and(|map| {
        [1, 7, 30].iter().all(|required_days| {
            map.keys()
                .any(|key| window_key_matches(key, *required_days))
        }) && map
            .iter()
            .any(|(key, window)| window_key_matches(key, days) && check(window))
    });
    let top_level_windows = value.as_object().is_some_and(|map| {
        map.iter()
            .any(|(key, value)| match key.to_lowercase().as_str() {
                "rolling_window" | "rolling_windows" => window_below_satisfies(value, days, check),
                "windows" => window_with_named_overall_satisfies(value, days, check),
                _ => false,
            })
    });
    direct_window_set || top_level_windows || overall_window_satisfies(value, days, check)
}

fn window_with_named_overall_satisfies<F>(value: &Value, days: u64, check: F) -> bool
where
    F: Fn(&Value) -> bool + Copy,
{
    value.as_object().is_some_and(|windows| {
        windows.iter().any(|(key, window)| {
            window_key_matches(key, days)
                && window.as_object().is_some_and(|fields| {
                    fields.iter().any(|(key, value)| {
                        matches!(
                            key.to_lowercase().as_str(),
                            "overall" | "global" | "summary" | "total"
                        ) && check(value)
                    })
                })
        })
    })
}

fn overall_window_satisfies<F>(value: &Value, days: u64, check: F) -> bool
where
    F: Fn(&Value) -> bool + Copy,
{
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_lowercase();
            let overall = ["overall", "global", "summary", "total"]
                .iter()
                .any(|marker| key.contains(marker));
            (overall && window_below_satisfies(value, days, check))
                || overall_window_satisfies(value, days, check)
        }),
        Value::Array(items) => items
            .iter()
            .any(|item| overall_window_satisfies(item, days, check)),
        _ => false,
    }
}

fn window_below_satisfies<F>(value: &Value, days: u64, check: F) -> bool
where
    F: Fn(&Value) -> bool + Copy,
{
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            (window_key_matches(key, days) && check(value))
                || window_below_satisfies(value, days, check)
        }),
        Value::Array(items) => items
            .iter()
            .any(|item| window_below_satisfies(item, days, check)),
        _ => false,
    }
}

fn counter_equals(value: &Value, names: &[&str], expected: f64) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_lowercase();
            (names.iter().any(|name| key.contains(name))
                && value
                    .as_f64()
                    .is_some_and(|number| (number - expected).abs() < 0.001))
                || counter_equals(value, names, expected)
        }),
        Value::Array(items) => items
            .iter()
            .any(|item| counter_equals(item, names, expected)),
        _ => false,
    }
}

fn window_key_matches(key: &str, days: u64) -> bool {
    let normalized: String = key
        .to_lowercase()
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '日' || *ch == '天')
        .collect();
    let patterns = match days {
        1 => [
            "1d", "1day", "day1", "daily1", "today", "今日", "1日", "1天",
        ],
        7 => ["7d", "7day", "day7", "daily7", "week", "近7", "7日", "7天"],
        30 => [
            "30d", "30day", "day30", "daily30", "month", "近30", "30日", "30天",
        ],
        _ => unreachable!(),
    };
    patterns.iter().any(|pattern| normalized.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_dashboard_public_spec_is_valid_json() {
        let spec = public_spec();
        assert!(spec.is_object());
        assert_eq!(spec["kit_id"], "token-dashboard-v0");
        assert!(spec.get("output_json_schema").is_some());
        assert!(spec.get("aggregation_semantics").is_some());
        assert!(spec.get("html_contract").is_some());
        assert!(spec.get("examples").is_some());
    }

    #[test]
    fn verify_rejects_invalid_json_output() {
        let result = verify_request_contract("not-json");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ACCEPTANCE_CONSTRAINT"));
    }

    #[test]
    fn verify_rejects_missing_rendered_field() {
        let result = verify_request_contract(r#"{"ok": true}"#);
        assert!(result.is_err());
    }

    #[test]
    fn source_policy_rejects_frozen_windows() {
        // The frozen source uses within_days in apply_event, which must be rejected
        let frozen = "pub fn initial_state() -> Value { json!({}) }
pub fn apply_event(state: &mut Value, event: &Value) {
    let _ = within_days(\"2026-07-15\", \"2026-07-15\", 30);
}
pub fn render_json(state: &Value, runtime: &Value) -> Value { json!({}) }
pub fn render_html(state: &Value, runtime: &Value) -> String { String::new() }";
        let result = verify_source_policy(frozen);
        assert!(
            result.is_err(),
            "expected error but got Ok: {result:?}"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("within_days") || error.contains("rolling windows"),
            "error should mention within_days or rolling windows but got: {error}"
        );
    }

    #[test]
    fn source_policy_rejects_host_clock() {
        let host_clock = r#"
pub fn initial_state() -> Value { json!({}) }
pub fn apply_event(state: &mut Value, event: &Value) { let _ = event; }
pub fn render_json(state: &Value, runtime: &Value) -> Value { let _ = today_utc(); json!({}) }
pub fn render_html(state: &Value, runtime: &Value) -> String { String::new() }
"#;
        assert!(verify_source_policy(host_clock)
            .unwrap_err()
            .contains("runtime.today_utc"));
    }
}
