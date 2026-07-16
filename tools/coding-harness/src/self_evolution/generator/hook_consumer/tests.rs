use super::*;
use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
use agent_core_kernel::domain::DevelopmentRequestDraft;
use std::path::PathBuf;
use std::process::Command;

fn request() -> DevelopmentRequest {
    let mut draft = DevelopmentRequestDraft::new(
        TargetKind::HookConsumerService,
        "request-driven-observer".into(),
    );
    draft.requirements = vec!["display observed facts".into()];
    draft.required_contracts = vec!["event.observe.v0".into()];
    draft.requested_permissions = vec!["journal.observe".into()];
    draft.acceptance_criteria = vec!["read-only service is healthy".into()];
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

fn source() -> &'static str {
    r#"pub fn initial_state() -> Value { json!({"count":0}) }
pub fn apply_event(state: &mut Value, event: &Value) { let _ = event; state["count"] = json!(state["count"].as_u64().unwrap_or(0) + 1); }
pub fn render_json(state: &Value, runtime: &Value) -> Value { json!({"state":state,"runtime":runtime}) }
pub fn render_html(state: &Value, runtime: &Value) -> String { let _ = (state, runtime); "<h1>Observed facts</h1>".to_string() }"#
}

fn temp_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "coding_generator_{label}_{}_{}",
        std::process::id(),
        unique_suffix()
    ))
}

#[test]
fn materialized_candidate_is_request_bound_and_replay_stable() {
    let root = temp_root("materialize");
    std::fs::create_dir_all(&root).unwrap();
    let request = request();
    let first = materialize(&root, "candidate-test", &request, source(), "model-test").unwrap();
    let second = load_existing(
        &request,
        "candidate-test",
        &root.join("candidate-test/candidate"),
    )
    .unwrap();
    assert_eq!(first["candidate_digest"], second["candidate_digest"]);
    assert_eq!(
        first["component_manifest"]["generation"]["kind"],
        "request-driven-model-module-v0"
    );
    assert_eq!(
        first["component_manifest"]["test_kit"],
        "hook-consumer-service-contract-v0"
    );
    assert!(!root
        .join("candidate-test/candidate/specification.json")
        .read_to_string()
        .unwrap()
        .contains("principal:test"));
    let build = Command::new("cargo")
        .args(["check", "--locked"])
        .current_dir(root.join("candidate-test/candidate"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "generated profile runtime failed to compile: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cached_candidate_rejects_fixed_runtime_and_manifest_tampering() {
    for tamper in ["runtime", "manifest"] {
        let root = temp_root(tamper);
        std::fs::create_dir_all(&root).unwrap();
        let request = request();
        materialize(&root, "candidate-test", &request, source(), "model-test").unwrap();
        let candidate = root.join("candidate-test/candidate");
        if tamper == "runtime" {
            std::fs::write(candidate.join("src/support.rs"), "pub fn bypass() {}").unwrap();
        } else {
            let path = candidate.join("manifest.json");
            let mut manifest: Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            manifest["requested_permissions"] = json!(["journal.observe", "host.execute"]);
            std::fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        }
        assert!(load_existing(&request, "candidate-test", &candidate).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn telemetry_request_contract_requires_dimensions_windows_and_runtime_metadata() {
    let mut telemetry_request = request();
    telemetry_request.requirements = vec!["Token usage dashboard".into()];
    let output = json!({
        "rendered": {
            "by_date": {"2026-07-15": {"input":10,"cached":2,"output":5,"reasoning":1,"latency":20,"failures":1}},
            "by_run": {"run-1": {}},
            "by_model": {"model-a": {}},
            "by_profile": {"default": {}},
            "today_1d": {},
            "last_7_days": {},
            "last_30_days": {},
            "overall": {
                "today_1d": {"calls": 2, "avg_latency_ms": 25, "unavailable": 1, "failures": 1},
                "last_7_days": {"calls": 2, "avg_latency_ms": 25, "unavailable": 1, "failures": 1},
                "last_30_days": {"calls": 2, "avg_latency_ms": 25, "unavailable": 1, "failures": 1}
            },
            "unavailable": {"input": 1},
            "telemetry_unavailable": false,
            "last_observed_cursor": 3,
            "projection_lag": "caught_up",
            "component_version": "0.1.0",
            "health": "ready"
        },
        "html_telemetry_metrics": true,
        "html_average_latency": true
    });
    assert!(validate_request_contract(&telemetry_request, &output.to_string()).is_ok());

    let mut missing_run = output.clone();
    missing_run["rendered"]["by_run"] = json!({"unknown": {}});
    let error =
        validate_request_contract(&telemetry_request, &missing_run.to_string()).unwrap_err();
    assert!(error.contains("run-1"));

    let mut rolling_windows = output;
    rolling_windows["rendered"]["overall"] = Value::Null;
    rolling_windows["rendered"]["rolling_windows"] = json!({
        "1day": {"calls": 2, "latency_avg": 25, "unavailable_count": 1, "failures": 1},
        "7day": {"calls": 2, "latency_avg": 25, "unavailable_count": 1, "failures": 1},
        "30day": {"calls": 2, "latency_avg": 25, "unavailable_count": 1, "failures": 1}
    });
    assert!(validate_request_contract(&telemetry_request, &rolling_windows.to_string()).is_ok());

    rolling_windows["rendered"]["rolling_windows"] = Value::Null;
    rolling_windows["rendered"]["windows"] = json!({
        "1_day": {"models": {"model-a": {"calls": 1}}, "total": {"calls": 2, "avg_latency_ms": 25, "unavailable_count": 1, "failures": 1}},
        "7_day": {"models": {"model-a": {"calls": 1}}, "total": {"calls": 2, "avg_latency_ms": 25, "unavailable_count": 1, "failures": 1}},
        "30_day": {"models": {"model-a": {"calls": 1}}, "total": {"calls": 2, "avg_latency_ms": 25, "unavailable_count": 1, "failures": 1}}
    });
    assert!(validate_request_contract(&telemetry_request, &rolling_windows.to_string()).is_ok());

    rolling_windows["rendered"]["windows"]["1_day"]["total"] = Value::Null;
    assert!(validate_request_contract(&telemetry_request, &rolling_windows.to_string()).is_err());

    rolling_windows["rendered"]["windows"] = Value::Null;
    for days in [1, 7, 30] {
        rolling_windows["rendered"][format!("{days}_day")] = json!({
            "calls": 2,
            "avg_latency_ms": 25,
            "unavailable_count": 1,
            "failures": 1
        });
    }
    assert!(validate_request_contract(&telemetry_request, &rolling_windows.to_string()).is_ok());

    rolling_windows["rendered"]["1_day"]["calls"] = json!(1);
    assert!(validate_request_contract(&telemetry_request, &rolling_windows.to_string()).is_err());
}

#[test]
fn combined_probe_reports_profile_and_request_failures_together() {
    let mut telemetry_request = request();
    telemetry_request.requirements = vec!["Token usage dashboard".into()];
    let output = json!({
        "ok": true,
        "schema_version": "hook-consumer-service-contract-v0",
        "events_applied": 3,
        "html_nonempty": true,
        "html_safe": true,
        "html_runtime_metadata": false,
        "html_telemetry_metrics": true,
        "html_average_latency": true,
        "rendered": {
            "by_date": {"2026-07-15": {"input":10,"cached":2,"output":5,"reasoning":1,"latency":50,"failures":1,"unavailable":1}},
            "by_run": {"run-1": {}},
            "by_model": {"model-a": {}},
            "by_profile": {"default": {}},
            "rolling_windows": {
                "1_day": {"calls":2,"latency_ms":50,"failures":1,"unavailable":1},
                "7_day": {"calls":2,"latency_ms":50,"failures":1,"unavailable":1},
                "30_day": {"calls":2,"latency_ms":50,"failures":1,"unavailable":1}
            },
            "telemetry_unavailable": false,
            "last_observed_cursor": 3,
            "projection_lag": "caught_up",
            "component_version": "0.1.0",
            "health": "ready"
        }
    });
    let error = validate_contracts(&telemetry_request, &output.to_string()).unwrap_err();
    assert!(error.contains("html_runtime_metadata"));
    assert!(error.contains("overall-1-day-average-latency=25"));
    assert!(error.contains("overall-30-day-average-latency=25"));
    assert!(error.contains("HTML_RUNTIME_METADATA_CONTRACT"));
}

#[test]
fn telemetry_request_source_rejects_frozen_ingest_time_windows() {
    let mut telemetry_request = request();
    telemetry_request.requirements = vec!["Token usage dashboard".into()];
    let frozen = source().replace(
        "let _ = event;",
        "let _ = within_days(\"2026-07-15\", \"2026-07-15\", 30); let _ = event;",
    );
    assert!(validate_request_source(&telemetry_request, &frozen)
        .unwrap_err()
        .contains("rolling windows"));
    assert!(validate_request_source(&telemetry_request, source()).is_ok());

    let host_clock = source().replace("let _ = event;", "let _ = today_utc(); let _ = event;");
    assert!(validate_request_source(&telemetry_request, &host_clock)
        .unwrap_err()
        .contains("runtime.today_utc"));
}

#[test]
fn repair_diagnostics_do_not_disclose_host_path_or_candidate_key() {
    let base = Path::new("/private/operator/artifacts/generated");
    let sanitized = sanitize_model_diagnostics(
        "/private/operator/artifacts/generated/.candidate-secret/src/component.rs",
        base,
        "candidate-secret",
    );
    assert_eq!(
        sanitized,
        "<generator-root>/.<candidate-id>/src/component.rs"
    );
}

#[test]
fn fixed_runtime_hard_bounds_projection_growth() {
    assert!(SUPPORT_RS.contains("MAX_KEYS_PER_OBJECT: usize = 2_048"));
    assert!(MAIN_RS.contains("MAX_PROJECTION_BYTES: usize = 16 * 1024 * 1024"));
    assert!(MAIN_RS.contains("component_projection_too_large"));
}

#[test]
fn unused_initial_attempts_extend_repairs_without_exceeding_six_model_calls() {
    assert_eq!(repair_budget(1), 4);
    assert_eq!(repair_budget(2), 4);
    assert_eq!(repair_budget(3), 3);
    for initial_attempts in 1..=3 {
        assert!(initial_attempts + repair_budget(initial_attempts) <= 6);
    }
}

trait ReadText {
    fn read_to_string(&self) -> std::io::Result<String>;
}

impl ReadText for PathBuf {
    fn read_to_string(&self) -> std::io::Result<String> {
        std::fs::read_to_string(self)
    }
}

/// Generic SYSTEM_PROMPT contains no Token Dashboard product terms.
#[test]
fn generic_prompt_contains_no_product_terms() {
    let forbidden = [
        "rolling_windows", "by_model", "by_profile", "run-1", "model-a",
        "input_tokens", "cached_tokens", "reasoning_tokens", "Token Dashboard",
    ];
    for term in &forbidden {
        assert!(
            !crate::self_evolution::generator::model::SYSTEM_PROMPT.contains(term),
            "generic prompt must not contain product term: {term}"
        );
        assert!(
            !crate::self_evolution::generator::model::SYSTEM_PROMPT.contains(&term.to_lowercase()),
            "generic prompt must not contain product term (lowercase): {term}"
        );
    }
}

/// Cross-request isolation: a non-Token request must not fail on Token-specific fields.
#[test]
fn non_token_request_does_not_fail_on_token_fields() {
    let mut generic_request = request();
    // A hook consumer that DOES NOT mention "token" in criteria
    generic_request.requirements = vec!["display failure events by category".into()];
    generic_request.acceptance_criteria = vec!["read-only failure page".into()];

    // Minimal valid output with only generic runtime metadata (no token fields)
    let output = json!({
        "rendered": {
            "events_by_category": {"timeout": 5, "error": 3},
            "telemetry_unavailable": false,
            "last_observed_cursor": 8,
            "projection_lag": "caught_up",
            "component_version": "0.1.0",
            "health": "ready"
        }
    });
    // Must pass validation without Token-specific fields
    assert!(
        validate_request_contract(&generic_request, &output.to_string()).is_ok(),
        "non-Token request must pass without Token Dashboard fields"
    );
}

/// Token Dashboard request must fail when Token fields are missing.
#[test]
fn token_request_requires_token_fields() {
    let mut token_request = request();
    token_request.requirements = vec!["Token usage dashboard".into()];

    let output = json!({
        "rendered": {
            "telemetry_unavailable": false,
            "last_observed_cursor": 3,
            "projection_lag": "caught_up",
            "component_version": "0.1.0",
            "health": "ready"
        }
    });
    let error = validate_request_contract(&token_request, &output.to_string()).unwrap_err();
    assert!(error.contains("GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED"));
    assert!(error.contains("run-1"));
    assert!(error.contains("model-a"));
}

/// Acceptance failure produces no side effects (tested via error code only).
#[test]
fn acceptance_failure_does_not_produce_candidate() {
    let mut token_request = request();
    token_request.requirements = vec!["Token usage dashboard".into()];
    let output = json!({"rendered": {"health": "ready", "telemetry_unavailable": false}});
    let error = validate_request_contract(&token_request, &output.to_string()).unwrap_err();
    assert!(
        error.contains("GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED"),
        "acceptance failure must use ACCEPTANCE_REPAIR_EXHAUSTED, not COMPILE"
    );
    assert!(
        !error.contains("COMPILE"),
        "acceptance failure must not be classified as compile failure"
    );
}
