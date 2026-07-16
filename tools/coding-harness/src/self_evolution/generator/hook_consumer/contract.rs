use super::truncate_diagnostics;
use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};

pub(super) fn validate_profile_contract(stdout: &str) -> Result<(), String> {
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
            truncate_diagnostics(stdout),
        ))
    }
}

pub(super) fn validate_contracts(request: &DevelopmentRequest, stdout: &str) -> Result<(), String> {
    let mut failures = Vec::new();
    if let Err(error) = validate_profile_contract(stdout) {
        failures.push(error);
    }
    if let Err(error) = validate_request_contract(request, stdout) {
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

pub(super) fn validate_request_contract(
    request: &DevelopmentRequest,
    stdout: &str,
) -> Result<(), String> {
    if !request_requires_model_telemetry(request) {
        return Ok(());
    }
    let output: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| "REQUEST_CONTRACT_OUTPUT_INVALID".to_string())?;
    let rendered = output
        .get("rendered")
        .ok_or_else(|| "REQUEST_CONTRACT_RENDERED_MISSING".to_string())?;
    let rendered_text = serde_json::to_string(rendered)
        .map_err(|_| "REQUEST_CONTRACT_RENDERED_INVALID".to_string())?
        .to_lowercase();
    let mut missing = Vec::new();
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
    if !has_positive_counter(rendered, &["unavailable"]) {
        missing.push("positive-unavailable-counter".into());
    }
    if !has_positive_counter(rendered, &["failure", "fail_count", "failures"]) {
        missing.push("positive-failure-counter".into());
    }
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
            counter_equals(
                window,
                &["avg_latency", "average_latency", "latency_avg"],
                25.0,
            )
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
            "REQUEST_CONTRACT_FAILED missing={}\nPATH_CONTRACT: run dimension comes from top-level event.run_id; model and profile dimensions come from event.payload.model and event.payload.profile.\nWINDOW_CONTRACT: expose overall/global/summary/total windows, a top-level rolling_windows object, or top-level windows whose distinct 1_day, 7_day, and 30_day objects each contain total/overall/summary/global. Each overall window must include calls=2, avg_latency_ms or latency_avg=25, failures=1, and a positive unavailable counter.\nRENDERED_OUTPUT:\n{}",
            missing.join(","),
            truncate_diagnostics(&rendered_text),
        ))
    }
}

pub(super) fn validate_request_source(
    request: &DevelopmentRequest,
    source: &str,
) -> Result<(), String> {
    if !request_requires_model_telemetry(request) {
        return Ok(());
    }
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

fn request_requires_model_telemetry(request: &DevelopmentRequest) -> bool {
    request
        .requirements
        .iter()
        .chain(request.acceptance_criteria.iter())
        .any(|value| value.to_lowercase().contains("token"))
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
