//! Token Dashboard private verifier.
//!
//! Validates that the generated module:
//! 1. Passes the profile contract (hook-consumer-service-contract-v0)
//! 2. Renders the required telemetry fields derived from the input events
//! 3. Computes values from input events, not hardcoded example values
//! 4. Follows source-level policies (no within_days in apply_event, no today_utc)
//!
//! Expected values are COMPUTED from the input at verification time.
//! No hardcoded assertions from public spec examples are used.

use super::super::constraint_diagnostic;
use super::super::shared_verifier_engine::{truncate_diagnostics, validate_events_applied};
use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Expected metrics computed from the private verification case input.
struct ExpectedMetrics {
    /// Business invocation events (completed + failed) — excludes unknown/other.
    invocation_events: usize,
    /// Completed invocations.
    completed_count: usize,
    /// Failed invocations.
    failed_count: usize,
    /// Invocations where any token field is null/missing (unavailable).
    unavailable_count: usize,
    /// Average latency in ms across all invocation events that have latency_ms.
    avg_latency: f64,
    /// Distinct run IDs from invocation events.
    run_ids: BTreeSet<String>,
    /// Distinct model names from invocation events.
    model_names: BTreeSet<String>,
    /// Distinct profile names from invocation events.
    profile_names: BTreeSet<String>,
    /// Distinct dates (YYYY-MM-DD) from invocation events.
    dates: BTreeSet<String>,
}

/// Compute expected metrics from the input events according to the public spec.
///
/// Rules (from public spec):
/// - `events_applied` counts ALL events (including unknown future types).
/// - Business call_count = completed + failed invocations.
/// - Unknown events do NOT contribute to call_count, dimensions, or aggregates.
/// - latency_ms on completed events contributes to avg_latency.
/// - latency_ms on failed events MAY contribute to avg_latency (spec allows it).
/// - A completed event with any null/missing token field counts as unavailable.
/// - Failed events do NOT count toward unavailable.
fn compute_expected_from_input(input: &Value) -> ExpectedMetrics {
    let events = input
        .get("events")
        .and_then(Value::as_array)
        .map(std::vec::Vec::as_slice)
        .unwrap_or(&[]);

    let mut completed_count = 0usize;
    let mut failed_count = 0usize;
    let mut unavailable_count = 0usize;
    let mut latency_sum_ms: f64 = 0.0;
    let mut latency_count: usize = 0;
    let mut run_ids: BTreeSet<String> = BTreeSet::new();
    let mut model_names: BTreeSet<String> = BTreeSet::new();
    let mut profile_names: BTreeSet<String> = BTreeSet::new();
    let mut dates: BTreeSet<String> = BTreeSet::new();

    for event in events {
        let kind = event
            .get("event_kind")
            .and_then(Value::as_str)
            .unwrap_or("");

        let is_completed = kind == "model.invocation.completed.v0";
        let is_failed = kind == "model.invocation.failed.v0";
        if !is_completed && !is_failed {
            continue; // unknown events — no business aggregation
        }

        if is_completed {
            completed_count += 1;
        }
        if is_failed {
            failed_count += 1;
        }

        // Extract run_id (top-level on envelope)
        if let Some(run_id) = event.get("run_id").and_then(Value::as_str) {
            run_ids.insert(run_id.to_string());
        }

        // Extract payload fields
        if let Some(payload) = event.get("payload") {
            if let Some(model) = payload.get("model").and_then(Value::as_str) {
                model_names.insert(model.to_string());
            }
            if let Some(profile) = payload.get("profile").and_then(Value::as_str) {
                profile_names.insert(profile.to_string());
            }

            // Latency — public spec says it's present for completed AND failed events
            if let Some(latency) = payload.get("latency_ms") {
                if let Some(ms) = latency.as_f64() {
                    latency_sum_ms += ms;
                    latency_count += 1;
                }
            }

            // Unavailable: a completed event where any token field is null/missing
            if is_completed {
                let token_fields = [
                    "input_tokens",
                    "cached_input_tokens",
                    "output_tokens",
                    "reasoning_tokens",
                    "total_tokens",
                ];
                let has_null_token = token_fields
                    .iter()
                    .any(|field| payload.get(*field).map_or(true, |v| v.is_null()));
                if has_null_token {
                    unavailable_count += 1;
                }
            }
        }

        // Date from occurred_at
        if let Some(occurred_at) = event.get("occurred_at").and_then(Value::as_str) {
            if occurred_at.len() >= 10 {
                dates.insert(occurred_at[..10].to_string());
            }
        }
    }

    let total_invocations = completed_count + failed_count;
    let avg_latency = if latency_count > 0 {
        latency_sum_ms / latency_count as f64
    } else {
        0.0
    };

    ExpectedMetrics {
        invocation_events: total_invocations,
        completed_count,
        failed_count,
        unavailable_count,
        avg_latency,
        run_ids,
        model_names,
        profile_names,
        dates,
    }
}

/// Private verifier for Token Dashboard.
pub fn verify(
    _request: &DevelopmentRequest,
    source: &str,
    input: &str,
    stdout: &str,
) -> Result<(), String> {
    let mut failures = Vec::new();

    // Events-applied check (shared logic — count comes from actual input)
    if let Err(error) = validate_events_applied(input, stdout) {
        failures.push(error);
    }

    // Parse input to determine if we have business events for deeper checks
    let input_value: Value = serde_json::from_str(input).unwrap_or(json!({}));
    let expected = compute_expected_from_input(&input_value);

    // Profile contract check (fields other than events_applied)
    if let Err(error) = verify_profile_contract(stdout) {
        failures.push(error);
    }

    // Request contract check (telemetry-specific fields)
    if expected.invocation_events > 0 {
        if let Err(error) = verify_request_contract(stdout, &expected) {
            failures.push(error);
        }
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
            truncate_diagnostics(stdout),
        ))
    }
}

/// Validate the rendered output for telemetry-specific contract fields.
///
/// Uses `expected` (computed from the actual input) to determine what
/// dimensions, counters, and metrics should appear in the output.
/// Diagnostics use constraint IDs rather than leaking exact expected values.
fn verify_request_contract(stdout: &str, expected: &ExpectedMetrics) -> Result<(), String> {
    let output: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| constraint_diagnostic("json.parse", "$", "valid JSON", "parse failure"))?;
    let rendered = output.get("rendered").ok_or_else(|| {
        constraint_diagnostic(
            "json.rendered.required",
            "$.rendered",
            "required object",
            "missing",
        )
    })?;
    let rendered_text = serde_json::to_string(rendered)
        .map_err(|_| "REQUEST_CONTRACT_RENDERED_INVALID".to_string())?
        .to_lowercase();
    let rendered_lower = rendered_text;

    let mut missing = Vec::new();

    // Generic runtime metadata
    for (label, aliases) in [
        ("telemetry_unavailable", &["telemetry_unavailable"][..]),
        ("last_observed_cursor", &["last_observed_cursor"][..]),
        ("projection_lag", &["projection_lag"][..]),
        ("component_version", &["component_version"][..]),
        ("health", &["health"][..]),
    ] {
        if !aliases.iter().any(|marker| rendered_lower.contains(marker)) {
            missing.push(format!("runtime-{label}"));
        }
    }

    // Dimensions derived from input: run IDs
    for run_id in &expected.run_ids {
        let lower = run_id.to_lowercase();
        if !rendered_lower.contains(&lower) {
            missing.push(format!("dim-run-{run_id}"));
        }
    }
    // Model names
    for model in &expected.model_names {
        let lower = model.to_lowercase();
        if !rendered_lower.contains(&lower) {
            missing.push(format!("dim-model-{model}"));
        }
    }
    // Profile names
    for profile in &expected.profile_names {
        let lower = profile.to_lowercase();
        if !rendered_lower.contains(&lower) {
            missing.push(format!("dim-profile-{profile}"));
        }
    }
    // Dates
    for date in &expected.dates {
        if !rendered_lower.contains(date) {
            missing.push(format!("dim-date-{date}"));
        }
    }

    // Token dimension labels
    for (label, marker) in [
        ("input", "input"),
        ("cached", "cached"),
        ("output", "output"),
        ("reasoning", "reasoning"),
        ("latency", "latency"),
    ] {
        if !rendered_lower.contains(marker) {
            missing.push(format!("dim-{label}"));
        }
    }

    // Failure dimension (if there are failures in the input)
    if expected.failed_count > 0 {
        let failure_markers = ["failure", "fail_count", "failures"];
        if !failure_markers.iter().any(|m| rendered_lower.contains(m)) {
            missing.push("dim-failure".into());
        }
    }

    // Unavailable dimension
    let unavailable_markers = ["unavailable"];
    if !unavailable_markers
        .iter()
        .any(|m| rendered_lower.contains(m))
    {
        missing.push("dim-unavailable".into());
    }

    // Positive counters
    if expected.failed_count > 0
        && !has_positive_counter(rendered, &["failure", "fail_count", "failures"])
    {
        missing.push("counter-failure".into());
    }
    if expected.unavailable_count > 0 && !has_positive_counter(rendered, &["unavailable"]) {
        missing.push("counter-unavailable".into());
    }

    // Rolling windows validation
    for days in [1, 7, 30] {
        if !has_window_key(rendered, days) {
            missing.push(format!("window-{days}day"));
        }
        if expected.invocation_events > 0 && !has_positive_overall_window(rendered, days) {
            missing.push(format!("pos-window-{days}day"));
        }
        // Check that overall windows have a positive unavailable counter
        // only when the input has unavailable events
        if expected.unavailable_count > 0
            && !requested_overall_window_satisfies(rendered, days, |window| {
                has_positive_counter(window, &["unavailable"])
            })
        {
            missing.push(format!("window-{days}day-unavailable"));
        }
        // Check that overall windows have a positive failure counter
        // only when the input has failure events
        if expected.failed_count > 0
            && !requested_overall_window_satisfies(rendered, days, |window| {
                has_positive_counter(window, &["failure", "fail_count", "failures"])
            })
        {
            missing.push(format!("window-{days}day-failure"));
        }
    }

    if output
        .get("html_telemetry_metrics")
        .and_then(Value::as_bool)
        != Some(true)
    {
        missing.push("html-telemetry-metrics".into());
    }
    if output.get("html_average_latency").and_then(Value::as_bool) != Some(true) {
        missing.push("html-average-latency".into());
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED missing={}",
            missing.join(","),
        ))
    }
}

/// Validate that the generated Rust source follows the Token Dashboard policy.
fn verify_source_policy(source: &str) -> Result<(), String> {
    let syntax =
        syn::parse_file(source).map_err(|_| "REQUEST_SOURCE_CONTRACT_INVALID_RUST".to_string())?;
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

// ---- Helper functions ----

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
    fn verify_rejects_invalid_json_output() {
        let result = verify_profile_contract("not-json");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("PROFILE_CONTRACT_OUTPUT_INVALID"));
    }

    #[test]
    fn verify_rejects_missing_rendered_field() {
        let result = verify_request_contract(
            r#"{"ok": true}"#,
            &ExpectedMetrics {
                invocation_events: 0,
                completed_count: 0,
                failed_count: 0,
                unavailable_count: 0,
                avg_latency: 0.0,
                run_ids: BTreeSet::new(),
                model_names: BTreeSet::new(),
                profile_names: BTreeSet::new(),
                dates: BTreeSet::new(),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn source_policy_rejects_frozen_windows() {
        let frozen = "pub fn initial_state() -> Value { json!({}) }
pub fn apply_event(state: &mut Value, event: &Value) {
    let _ = within_days(\"2026-07-15\", \"2026-07-15\", 30);
}
pub fn render_json(state: &Value, runtime: &Value) -> Value { json!({}) }
pub fn render_html(state: &Value, runtime: &Value) -> String { String::new() }";
        let result = verify_source_policy(frozen);
        assert!(result.is_err(), "expected error but got Ok: {result:?}");
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

    #[test]
    fn compute_expected_from_invocation_events() {
        let input: Value = serde_json::from_str(r#"{"events":[
            {"event_id":"c1","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-a","payload":{"model":"m1","profile":"p1","latency_ms":100,"input_tokens":10,"cached_input_tokens":2,"output_tokens":5,"reasoning_tokens":1,"total_tokens":15}},
            {"event_id":"c2","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-16T14:00:00Z","run_id":"run-b","payload":{"model":"m2","profile":"p2","latency_ms":200,"input_tokens":20,"cached_input_tokens":3,"output_tokens":10,"reasoning_tokens":2,"total_tokens":30}},
            {"event_id":"f1","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-15T11:00:00Z","run_id":"run-a","payload":{"model":"m1","profile":"p1","error_category":"timeout"}},
            {"event_id":"unk","event_kind":"future.unknown.v1","occurred_at":"2026-07-18T00:00:00Z","payload":{"x":1}}
        ]}"#).unwrap();
        let metrics = compute_expected_from_input(&input);
        assert_eq!(metrics.invocation_events, 3, "completed+failed=3");
        assert_eq!(metrics.completed_count, 2);
        assert_eq!(metrics.failed_count, 1);
        assert_eq!(metrics.unavailable_count, 0); // all token fields present and non-null
        assert!((metrics.avg_latency - 150.0).abs() < 0.001); // (100+200)/2
        assert!(metrics.run_ids.contains("run-a"));
        assert!(metrics.run_ids.contains("run-b"));
        assert!(metrics.model_names.contains("m1"));
        assert!(metrics.model_names.contains("m2"));
        assert!(metrics.profile_names.contains("p1"));
        assert!(metrics.profile_names.contains("p2"));
        assert!(metrics.dates.contains("2026-07-15"));
        assert!(metrics.dates.contains("2026-07-16"));
    }

    #[test]
    fn compute_expected_unavailable_from_null_tokens() {
        let input: Value = serde_json::from_str(r#"{"events":[
            {"event_id":"c1","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-a","payload":{"model":"m1","profile":"p1","latency_ms":100,"input_tokens":null,"output_tokens":5,"total_tokens":null}}
        ]}"#).unwrap();
        let metrics = compute_expected_from_input(&input);
        assert_eq!(metrics.unavailable_count, 1);
        assert!((metrics.avg_latency - 100.0).abs() < 0.001);
    }

    #[test]
    fn unknown_events_do_not_affect_business_aggregates() {
        let input: Value = serde_json::from_str(r#"{"events":[
            {"event_id":"unk","event_kind":"future.observed.v99","occurred_at":"2026-07-18T00:00:00Z","payload":{"x":1}},
            {"event_id":"c1","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-a","payload":{"model":"m1","profile":"p1","latency_ms":50,"input_tokens":5,"output_tokens":3,"total_tokens":8}}
        ]}"#).unwrap();
        let metrics = compute_expected_from_input(&input);
        // Unknown event does NOT count as invocation
        assert_eq!(metrics.invocation_events, 1);
        assert_eq!(metrics.completed_count, 1);
        assert_eq!(metrics.failed_count, 0);
        assert_eq!(metrics.run_ids.len(), 1);
        assert_eq!(metrics.model_names.len(), 1);
        // But events_applied (separate check) counts ALL events including unknown
        assert_eq!(
            input.get("events").and_then(Value::as_array).unwrap().len(),
            2,
            "events_applied counts all events including unknown"
        );
    }
}
