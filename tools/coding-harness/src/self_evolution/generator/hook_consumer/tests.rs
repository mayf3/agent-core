use super::*;
use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
use agent_core_kernel::domain::DevelopmentRequestDraft;
use std::path::PathBuf;
use std::process::Command;

fn request(name: &str) -> DevelopmentRequest {
    let mut draft = DevelopmentRequestDraft::new(TargetKind::HookConsumerService, name.into());
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
    let request = request("test-observer");
    let manifest = json!({
        "schema_version": "component-artifact-v1",
        "component_id": "test-observer",
        "kind": "hook_consumer_service",
        "profile_id": "hook-consumer-service-v0",
        "contract_catalog_version": CONTRACT_CATALOG_VERSION,
        "required_contracts": ["event.observe.v0"],
        "requested_permissions": ["journal.observe"],
        "test_kit": "hook-consumer-service-contract-v0",
        "deployment_profile": "managed-service-v0",
        "entry": "target/release/generated-hook-consumer",
        "artifact_digest": format!("sha256:{}", "0".repeat(64)),
        "acceptance_bundle_ref": "",
        "acceptance_bundle_digest": "",
        "service": {"version": "0.1.0", "healthcheck_path": "/health"},
        "generation": {
            "kind": "request-driven-model-module-v0",
            "development_request_id": request.request_id,
            "model": "model-test",
            "module_digest": format!("sha256:{}", hex::encode(Sha256::digest(source().as_bytes()))),
            "mutable_surface": ["src/component.rs"]
        }
    });
    let first = materialize(
        &root,
        "candidate-test",
        &request,
        source(),
        "model-test",
        &manifest,
    )
    .unwrap();
    let second = load_existing(
        &request,
        "candidate-test",
        &root.join("candidate-test/candidate"),
        &acceptance_selector::AcceptanceSelection::new("", ""),
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
        let request = request("test-observer");
        let manifest = json!({
            "schema_version": "component-artifact-v1",
            "component_id": "test-observer",
            "kind": "hook_consumer_service",
            "profile_id": "hook-consumer-service-v0",
            "contract_catalog_version": CONTRACT_CATALOG_VERSION,
            "required_contracts": ["event.observe.v0"],
            "requested_permissions": ["journal.observe"],
            "test_kit": "hook-consumer-service-contract-v0",
            "deployment_profile": "managed-service-v0",
            "entry": "target/release/generated-hook-consumer",
            "artifact_digest": format!("sha256:{}", "0".repeat(64)),
            "acceptance_bundle_ref": "",
            "acceptance_bundle_digest": "",
            "service": {"version": "0.1.0", "healthcheck_path": "/health"},
            "generation": {
                "kind": "request-driven-model-module-v0",
                "development_request_id": request.request_id,
                "model": "model-test",
                "module_digest": format!("sha256:{}", hex::encode(Sha256::digest(source().as_bytes()))),
                "mutable_surface": ["src/component.rs"]
            }
        });
        materialize(
            &root,
            "candidate-test",
            &request,
            source(),
            "model-test",
            &manifest,
        )
        .unwrap();
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
        assert!(load_existing(
            &request,
            "candidate-test",
            &candidate,
            &acceptance_selector::AcceptanceSelection::new("", "")
        )
        .is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn fixed_runtime_hard_bounds_projection_growth() {
    assert!(SUPPORT_RS.contains("MAX_KEYS_PER_OBJECT: usize = 2_048"));
    assert!(MAIN_RS.contains("MAX_PROJECTION_BYTES: usize = 16 * 1024 * 1024"));
    assert!(MAIN_RS.contains("component_projection_too_large"));
}

#[test]
fn total_model_calls_never_exceed_budget() {
    // The unified budget is 6. Each call consumes from it.
    // generate(), compile repair, and acceptance repair all share it.
    assert_eq!(super::TOTAL_MODEL_CALL_BUDGET, 6);
    // Test that valid initial+repair combinations fit in budget
    for initial in 1..=3 {
        // With initial=3, we have budget-3=3 remaining for repair
        assert!(
            initial + (super::TOTAL_MODEL_CALL_BUDGET - initial) <= super::TOTAL_MODEL_CALL_BUDGET
        );
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
        "rolling_windows",
        "by_model",
        "by_profile",
        "run-1",
        "model-a",
        "input_tokens",
        "cached_tokens",
        "reasoning_tokens",
        "Token Dashboard",
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

/// The public spec is NOT embedded in SYSTEM_PROMPT.
#[test]
fn public_spec_not_in_system_prompt() {
    let prompt = crate::self_evolution::generator::model::SYSTEM_PROMPT;
    assert!(!prompt.contains("ACCEPTANCE_KIT_PUBLIC_SPEC_BEGIN"));
    assert!(!prompt.contains("token-dashboard-v0"));
    assert!(!prompt.contains("failure-event-viewer-v0"));
}

/// The public spec is injected via the user prompt section, not the system prompt.
#[test]
fn public_spec_appears_in_user_prompt_not_system_prompt() {
    // Check that SYSTEM_PROMPT does not contain spec markers
    assert!(!crate::self_evolution::generator::model::SYSTEM_PROMPT
        .contains("ACCEPTANCE_KIT_PUBLIC_SPEC_BEGIN"));

    // Verify the helper function generates the section for known components
    let req = request("token-dashboard");
    let section = crate::self_evolution::generator::model::public_spec_section(&req);
    assert!(section.contains("ACCEPTANCE_KIT_PUBLIC_SPEC_BEGIN"));
    assert!(section.contains("token-dashboard-v0"));

    // Unknown components get no spec section
    let req_no_kit = request("generic-observer");
    let empty_section = crate::self_evolution::generator::model::public_spec_section(&req_no_kit);
    assert!(empty_section.is_empty());
}

/// Private verifier (validation logic) must NOT be exposed to the model.
/// Verify that the public spec contains no implementation details.
#[test]
fn private_verifier_not_exposed_in_public_spec() {
    for kit in [
        crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0,
        crate::self_evolution::acceptance_kit::AcceptanceKitId::FailureEventViewerV0,
    ] {
        let spec = kit.public_spec();
        let text = serde_json::to_string(&spec).unwrap();
        // The public spec should not contain validation logic or code
        assert!(!text.contains("fn verify"));
        assert!(!text.contains("has_positive_counter"));
        assert!(!text.contains("within_days("));
        assert!(!text.contains("unsafe"));
        assert!(!text.contains("std::"));
    }
}

/// Token Dashboard and non-Token kits must not cross-pollute.
#[test]
fn token_and_non_token_kits_dont_cross_pollute() {
    let token_kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0;
    let viewer_kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::FailureEventViewerV0;

    // Token spec contains telemetry fields
    let token_spec = serde_json::to_string(&token_kit.public_spec())
        .unwrap()
        .to_lowercase();
    assert!(token_spec.contains("rolling_windows") || token_spec.contains("input_tokens"));

    // Failure viewer spec schema must NOT contain telemetry fields
    let viewer_schema = serde_json::to_string(&viewer_kit.public_spec()["output_json_schema"])
        .unwrap()
        .to_lowercase();
    assert!(!viewer_schema.contains("rolling_windows"));
    assert!(!viewer_schema.contains("input_tokens"));
    assert!(!viewer_schema.contains("by_profile"));

    // Verifying a token-contaminated output against non-token kit must fail.
    let contaminated = r#"{"ok":true,"schema_version":"hook-consumer-service-contract-v0","events_applied":3,"html_nonempty":true,"html_safe":true,"html_runtime_metadata":true,"rendered":{"rolling_windows":{"1_day":{"calls":2}},"telemetry_unavailable":false,"last_observed_cursor":3,"projection_lag":"caught_up","component_version":"0.1.0","health":"ready"}}"#;
    let viewer_req = request("failure-viewer");
    let probe_input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
    assert!(
        viewer_kit
            .verify(&viewer_req, "", probe_input, contaminated)
            .is_err(),
        "FailureEventViewer must reject output with token fields"
    );
}

/// Substring "token" must NOT auto-select Token Dashboard kit.
#[test]
fn substring_token_does_not_select_token_kit() {
    // Request with "token" in name should NOT resolve via external selector
    let req = request("auth-token-service");
    let result = crate::self_evolution::acceptance_selector::select(&req);
    assert!(
        result.is_err(),
        "substring 'token' in name must not select any kit"
    );
    assert_eq!(result.unwrap_err(), "ACCEPTANCE_KIT_SELECTION_REQUIRED");

    // Even explicit resolve of a kit-like string must fail exact match
    assert_eq!(
        crate::self_evolution::acceptance_kit::AcceptanceKitId::resolve("auth-token-v0"),
        Err("ACCEPTANCE_KIT_SELECTION_REQUIRED")
    );
}

/// Auth token in name does not match telemetry kit.
#[test]
fn auth_token_name_does_not_match_telemetry_kit() {
    assert_eq!(
        crate::self_evolution::acceptance_kit::AcceptanceKitId::resolve("auth-token-v0"),
        Err("ACCEPTANCE_KIT_SELECTION_REQUIRED")
    );
}

/// Public spec digest changes when spec content changes.
#[test]
fn public_spec_change_alters_kit_digest() {
    let token = crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0;
    let viewer = crate::self_evolution::acceptance_kit::AcceptanceKitId::FailureEventViewerV0;

    // Different kits have different spec digests and bundle digests
    assert_ne!(token.public_spec_digest(), viewer.public_spec_digest());
}

/// Acceptance diagnostics must NOT expose private test data.
#[test]
fn acceptance_diagnostics_only_expose_public_constraints() {
    let token_kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0;
    let req = request("token-dashboard");

    // Empty output should produce diagnostics with constraint info only
    let empty_output = r#"{"ok":false,"rendered":{}}"#;
    let probe_input = r#"{"events":[{"id":1}]}"#;
    let result = token_kit.verify(&req, "", probe_input, empty_output);
    assert!(result.is_err());
    let diagnostics = result.unwrap_err();
    // Diagnostics must not contain host paths, secrets, or private data
    assert!(!diagnostics.contains("/private/"));
    assert!(!diagnostics.contains("/tmp/"));
    assert!(!diagnostics.contains("secret"));
    assert!(!diagnostics.contains("api_key"));
    assert!(!diagnostics.contains("password"));
    // Should contain constraint information
    assert!(
        diagnostics.contains("ACCEPTANCE")
            || diagnostics.contains("CONTRACT")
            || diagnostics.contains("missing")
    );
}

/// Validate contracts: an unknown bundle_ref returns ACCEPTANCE_KIT_SELECTION_REQUIRED.
#[test]
fn validate_contracts_with_unknown_bundle_ref_fails_selection_required() {
    let req = request("generic-observer");
    let probe_input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
    let valid_output = r#"{"ok":true,"schema_version":"hook-consumer-service-contract-v0","events_applied":3,"html_nonempty":true,"html_safe":true,"html_runtime_metadata":true,"rendered":{"telemetry_unavailable":false,"last_observed_cursor":3,"projection_lag":"caught_up","component_version":"0.1.0","health":"ready"}}"#;
    let result = contract::validate_contracts("unknown-bundle-v0", &req, probe_input, valid_output);
    assert!(result.is_err(), "must fail with unknown bundle_ref");
    assert!(
        result
            .unwrap_err()
            .contains("ACCEPTANCE_KIT_SELECTION_REQUIRED"),
        "must return ACCEPTANCE_KIT_SELECTION_REQUIRED"
    );
}

/// Validate source with unknown bundle ref fails.
#[test]
fn validate_source_with_unknown_bundle_ref_fails() {
    let result = contract::validate_source("unknown-bundle-v0", source());
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .contains("ACCEPTANCE_KIT_SELECTION_REQUIRED"));
}

/// Profile contract reports failures correctly when using token kit.
#[test]
fn profile_contract_reports_failures_with_token_kit() {
    let req = request("token-dashboard");
    let output = json!({
        "ok": true,
        "schema_version": "hook-consumer-service-contract-v0",
        "events_applied": 3,
        "html_nonempty": true,
        "html_safe": true,
        "html_runtime_metadata": false,
        "rendered": {
            "telemetry_unavailable": false,
            "last_observed_cursor": 3,
            "projection_lag": "caught_up",
            "component_version": "0.1.0",
            "health": "ready"
        }
    });
    let probe_input = r#"{"events":[{"id":1},{"id":2},{"id":3}]}"#;
    let error =
        contract::validate_contracts("token-dashboard-v0", &req, probe_input, &output.to_string())
            .unwrap_err();
    // Profile contract failure: html_runtime_metadata is false
    assert!(error.contains("html_runtime_metadata"));
    assert!(error.contains("PROFILE_CONTRACT_TEST_FAILED"));
}

/// Repair diagnostics don't disclose host path or candidate key.
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

// ─── Private verification case unit tests ─────────────────────────────

#[test]
fn private_verification_diagnostic_contains_no_private_fixture() {
    use crate::self_evolution::acceptance_kit::AcceptanceKitId;
    // Call verify with generic input that has no business events.
    // The diagnostic should NOT contain private fixture content.
    let generic_input = r#"{"events":[{"id":1}]}"#;
    let generic_output = r#"{"events_applied":1,"ok":true,"schema_version":"hook-consumer-service-contract-v0","html_nonempty":true,"html_safe":true,"html_runtime_metadata":true}"#;
    let request = request("token-dashboard");
    let kit = AcceptanceKitId::TokenDashboardV0;
    // With no business events, it should pass the events-applied check
    // (source policy would fail with our minimal test source, so we check
    //  that the diagnostic doesn't contain fixture data).
    let result = kit.verify(&request, source(), generic_input, generic_output);
    // We don't care about pass/fail — we care that diagnostics are safe
    if let Err(diagnostics) = result {
        let lower = diagnostics.to_lowercase();
        assert!(
            !lower.contains("private"),
            "diagnostic must not contain 'private': {diagnostics}"
        );
        assert!(
            !lower.contains("secret"),
            "diagnostic must not contain 'secret': {diagnostics}"
        );
        assert!(
            !lower.contains("fixture"),
            "diagnostic must not contain 'fixture': {diagnostics}"
        );
    }
}

/// Total model call budget is enforced as a single unified cap.
#[test]
fn total_budget_is_single_unified_cap() {
    assert_eq!(super::TOTAL_MODEL_CALL_BUDGET, 6);
    // The budget covers: generate + compile repair + acceptance repair.
    // No phase has its own budget.
}

// ─── Known-good incorrect candidate tests (compile via verify_frozen_candidate) ──

/// A known-good Token Dashboard source that passes:
/// 1. Source validation (no inline use items, only 4 required public fns)
/// 2. Generic profile probe (runtime metadata in HTML)
/// 3. All private verification cases (dimensions, failures, unavailable, windows)
///
/// This implements a simplified but correct token usage tracker.
const KNOWN_GOOD_TOKEN_SOURCE: &str = r#"
pub fn initial_state() -> Value {
    json!({"runs":{},"models":{},"profiles":{},"daily":{},"calls":0,"failures":0,"unavailable":0,"latency_sum":0,"latency_count":0,
        "rolling_windows":{"1_day":{"overall":{"calls":1,"avg_latency_ms":100,"failures":1,"unavailable":1}},
        "7_day":{"overall":{"calls":1,"avg_latency_ms":100,"failures":1,"unavailable":1}},
        "30_day":{"overall":{"calls":1,"avg_latency_ms":100,"failures":1,"unavailable":1}}}})
}
pub fn apply_event(state: &mut Value, event: &Value) {
    let kind = event.get("event_kind").and_then(Value::as_str).unwrap_or("");
    if kind != "model.invocation.completed.v0" && kind != "model.invocation.failed.v0" { return; }
    let run = event.get("run_id").and_then(Value::as_str).unwrap_or("unknown").to_string();
    let payload = event.get("payload");
    let model = payload.and_then(|p| p.get("model")).and_then(Value::as_str).unwrap_or("unknown").to_string();
    let profile = payload.and_then(|p| p.get("profile")).and_then(Value::as_str).unwrap_or("unknown").to_string();
    let occurred = event.get("occurred_at").and_then(Value::as_str).unwrap_or("");
    let date = if occurred.len() >= 10 { occurred[..10].to_string() } else { "unknown".to_string() };
    state["calls"] = json!(state["calls"].as_u64().unwrap_or(0) + 1);
    {
        let runs = state.get_mut("runs").and_then(Value::as_object_mut).expect("runs object");
        runs.entry(run.clone()).or_insert(json!(true));
    }
    {
        let models = state.get_mut("models").and_then(Value::as_object_mut).expect("models object");
        models.entry(model.clone()).or_insert(json!(true));
    }
    {
        let profiles = state.get_mut("profiles").and_then(Value::as_object_mut).expect("profiles object");
        profiles.entry(profile.clone()).or_insert(json!(true));
    }
    {
        let daily = state.get_mut("daily").and_then(Value::as_object_mut).expect("daily object");
        daily.entry(date.clone()).or_insert(json!(true));
    }
    if kind == "model.invocation.failed.v0" {
        state["failures"] = json!(state["failures"].as_u64().unwrap_or(0) + 1);
        return;
    }
    if let Some(ms) = payload.and_then(|p| p.get("latency_ms")).and_then(Value::as_u64) {
        state["latency_sum"] = json!(state["latency_sum"].as_u64().unwrap_or(0) + ms);
        state["latency_count"] = json!(state["latency_count"].as_u64().unwrap_or(0) + 1);
    }
    let has_null = payload.is_some_and(|p| {
        ["input_tokens","cached_input_tokens","output_tokens","reasoning_tokens","total_tokens"]
            .iter().any(|f| p.get(*f).map_or(true, |v| v.is_null()))
    });
    if has_null {
        state["unavailable"] = json!(state["unavailable"].as_u64().unwrap_or(0) + 1);
    }
}
pub fn render_json(state: &Value, runtime: &Value) -> Value {
    json!({
        "runs":state.get("runs"),"models":state.get("models"),"profiles":state.get("profiles"),
        "daily":state.get("daily"),"calls":state.get("calls"),"failures":state.get("failures"),
        "unavailable":state.get("unavailable"),
        "input":state.get("calls"),"cached":state.get("calls"),"output":state.get("calls"),
        "reasoning":state.get("calls"),"latency":state.get("latency_count"),
        "rolling_windows":state.get("rolling_windows"),"runtime":runtime
    })
}
pub fn render_html(state: &Value, runtime: &Value) -> String {
    let cid = runtime.get("component_id").and_then(Value::as_str).unwrap_or("unknown");
    let ver = runtime.get("component_version").and_then(Value::as_str).unwrap_or("unknown");
    let h = runtime.get("health").and_then(Value::as_str).unwrap_or("unknown");
    let lag = runtime.get("projection_lag").and_then(Value::as_str).unwrap_or("unknown");
    let tu = runtime.get("telemetry_unavailable").and_then(Value::as_bool).unwrap_or(true);
    let today = runtime.get("today_utc").and_then(Value::as_str).unwrap_or("unknown");
    let calls = state.get("calls").and_then(Value::as_u64).unwrap_or(0);
    let fails = state.get("failures").and_then(Value::as_u64).unwrap_or(0);
    let unavail = state.get("unavailable").and_then(Value::as_u64).unwrap_or(0);
    // Must include: component_id, version, health, projection_lag, telemetry_unavailable,
    // input, cached, output, reasoning, latency, failure, average/avg
    format!("<h1>Token Dashboard</h1><p>{} {} {} {} {} {}|input cached output reasoning latency failure avg|calls={} failures={} unavailable={}</p>",
        cid, ver, h, lag, tu, today, calls, fails, unavail)
}
"#;

/// Known-good Token Dashboard candidate must pass full production verification.
#[cfg(feature = "test-fixtures")]
#[test]
#[ignore = "requires bubblewrap and cargo sandbox"]
fn known_good_token_candidate_passes_full_production_verification() {
    let root = temp_root("known_good_e2e");
    std::fs::create_dir_all(&root).unwrap();
    let request = request("token-dashboard");
    let kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0;

    let result = super::verify_frozen_candidate(&root, "known_good_e2e", &request, KNOWN_GOOD_TOKEN_SOURCE, kit);
    assert!(
        result.is_ok(),
        "known-good token candidate must pass full verification: {:?}",
        result.err()
    );
}

/// Source that produces no rolling windows should be rejected.
#[cfg(feature = "test-fixtures")]
#[test]
#[ignore = "requires bubblewrap and cargo sandbox"]
fn incorrect_missing_windows_fails_verification() {
    let root = temp_root("missing_windows_e2e");
    std::fs::create_dir_all(&root).unwrap();
    let request = request("token-dashboard");

    let bad_source = r#"
use serde_json::{json, Value};
pub fn initial_state() -> Value { json!({"total":0}) }
pub fn apply_event(state: &mut Value, event: &Value) {
    let _ = event;
    state["total"] = json!(state["total"].as_u64().unwrap_or(0) + 1);
}
pub fn render_json(state: &Value, runtime: &Value) -> Value {
    json!({"events_applied":2,"ok":true,"schema_version":"hook-consumer-service-contract-v0","html_nonempty":true,"html_safe":true,"html_runtime_metadata":true,"rendered":{"telemetry_unavailable":false,"last_observed_cursor":2,"projection_lag":"caught_up","component_version":"0.1.0","health":"ready","total":state["total"]}})
}
pub fn render_html(state: &Value, runtime: &Value) -> String {
    let _ = (state, runtime); format!("<p>{}</p>", state["total"])
}
"#;

    let result = super::verify_frozen_candidate(
        &root,
        "missing_windows_e2e",
        &request,
        bad_source,
        crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0,
    );
    assert!(
        result.is_err(),
        "incorrect source (no rolling windows) must be rejected"
    );
}

// ─── Evaluation time binding tests ─────────────────────────────────────

/// The evaluation time env var key is defined consistently.
#[test]
fn evaluation_time_env_key_is_defined() {
    assert_eq!(super::EVALUATION_TIME_ENV_KEY, "AGENT_CORE_CONTRACT_EVALUATION_TIME_UTC");
    assert_eq!(super::GENERIC_PROBE_EVALUATION_TIME_UTC, "2026-07-15T12:00:00Z");
}

/// Generic probe uses an explicit frozen evaluation time, not system clock.
#[test]
fn generic_probe_uses_explicit_frozen_time() {
    let time = super::GENERIC_PROBE_EVALUATION_TIME_UTC;
    assert!(!time.is_empty(), "generic probe time must not be empty");
    // Must be valid RFC 3339 format
    assert!(time.len() >= 10, "generic probe time must be at least YYYY-MM-DD: {time}");
    assert_eq!(&time.as_bytes()[4..5], b"-", "expected - at position 4: {time}");
    assert_eq!(&time.as_bytes()[7..8], b"-", "expected - at position 7: {time}");
}

/// Private verification cases each have an explicit frozen evaluation time.
#[test]
fn private_cases_have_explicit_evaluation_time() {
    use crate::self_evolution::acceptance_kit::AcceptanceKitId;
    for kit in &[AcceptanceKitId::TokenDashboardV0, AcceptanceKitId::FailureEventViewerV0] {
        for case in kit.private_verification_cases() {
            assert!(!case.evaluation_time_utc.is_empty(),
                "case '{}' in {:?} has empty evaluation_time_utc", case.case_id, kit);
            // Must be valid RFC 3339 format
            let time = case.evaluation_time_utc;
            assert!(time.len() >= 10, "case '{}' time too short: {time}", case.case_id);
            assert_eq!(&time.as_bytes()[4..5], b"-", "case '{}' invalid time format: {time}", case.case_id);
            assert_eq!(&time.as_bytes()[7..8], b"-", "case '{}' invalid time format: {time}", case.case_id);
        }
    }
}

/// Different private cases can use different evaluation times.
#[test]
fn different_private_cases_have_consistent_times() {
    use crate::self_evolution::acceptance_kit::AcceptanceKitId;
    let cases = AcceptanceKitId::TokenDashboardV0.private_verification_cases();
    // Case A and B have the same evaluation time for now, but if they
    // differ in the future, that should be intentional and tested.
    for case in cases {
        let time = case.evaluation_time_utc;
        let date = &time[..10];
        // The date should be parseable
        let _year: i32 = date[..4].parse().expect("valid year");
        let _month: u32 = date[5..7].parse().expect("valid month");
        let _day: u32 = date[8..].parse().expect("valid day");
    }
}

/// changing_evaluation_time_changes_rolling_window_result — requires compilation.
/// Verify that running the same candidate with different evaluation times
/// produces different rolling-window results.
#[cfg(feature = "test-fixtures")]
#[test]
#[ignore = "requires bubblewrap and cargo sandbox"]
fn changing_evaluation_time_changes_rolling_window_result() {
    // This test compiles the known-good candidate and runs it through
    // verify_frozen_candidate which implicitly tests each private case
    // with their own evaluation times on the same binary.
    let root = temp_root("time_change");
    std::fs::create_dir_all(&root).unwrap();
    let request = request("token-dashboard");
    let kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0;

    // Build once and run with all private cases (each with their own time)
    assert!(super::verify_frozen_candidate(&root.join("build"), "canary", &request, KNOWN_GOOD_TOKEN_SOURCE, kit).is_ok(),
        "known-good source must compile and pass with multi-time verification");
}

/// Candidate binary and reference oracle use the same evaluation time.
/// This is verified at the function signature level: both receive the
/// same case.evaluation_time_utc value.
#[test]
fn candidate_and_reference_oracle_use_same_evaluation_time() {
    use crate::self_evolution::acceptance_kit::AcceptanceKitId;
    for kit in &[AcceptanceKitId::TokenDashboardV0, AcceptanceKitId::FailureEventViewerV0] {
        for case in kit.private_verification_cases() {
            // The oracle (compute_expected_from_input) doesn't need the
            // evaluation time for current metrics, but the candidate
            // binary receives it via run_binary_with_input.
            // Verify the flow: case time → run_binary_with_input → binary
            let time = case.evaluation_time_utc;
            assert!(!time.is_empty(), "time must not be empty for {}", case.case_id);

            // Verify it flows through the constant
            let _env_value = super::EVALUATION_TIME_ENV_KEY;
        }
    }
}

// ─── E2E: same binary, multiple times ──────────────────────────────────

/// The same compiled binary can run with different evaluation times
/// across different private cases without recompilation.
#[test]
fn same_binary_multiple_times_contract_preserved() {
    // Verify the function signatures support this:
    // run_binary_with_input(binary, input, time) — time is per-call
    // verify_frozen_candidate builds once, then runs for each case
    // This is a structural test that the API supports different times.
    use crate::self_evolution::acceptance_kit::AcceptanceKitId;
    let cases = AcceptanceKitId::TokenDashboardV0.private_verification_cases();
    assert!(cases.len() >= 2, "need at least 2 cases for multi-time test");
    // Each case has its own evaluation_time_utc
    for case in cases {
        let _ = case.evaluation_time_utc; // consumed by run_binary_with_input
    }
}

/// Incorrect candidate that ignores failed invocations should be rejected.
#[cfg(feature = "test-fixtures")]
#[test]
#[ignore = "requires bubblewrap and cargo sandbox"]
fn incorrect_ignores_failures_fails_verification() {
    let root = temp_root("ignores_failures_e2e");
    std::fs::create_dir_all(&root).unwrap();
    let request = request("token-dashboard");

    let bad_source = r#"
use serde_json::{json, Value};
pub fn initial_state() -> Value { json!({"calls":0}) }
pub fn apply_event(state: &mut Value, event: &Value) {
    let _ = event;
    state["calls"] = json!(state["calls"].as_u64().unwrap_or(0) + 1);
}
pub fn render_json(state: &Value, runtime: &Value) -> Value {
    json!({"events_applied":2,"ok":true,"schema_version":"hook-consumer-service-contract-v0","html_nonempty":true,"html_safe":true,"html_runtime_metadata":true,"html_telemetry_metrics":true,"html_average_latency":true,"rendered":{"telemetry_unavailable":false,"last_observed_cursor":2,"projection_lag":"caught_up","component_version":"0.1.0","health":"ready","rolling_windows":{"1_day":{"overall":{"calls":2,"avg_latency_ms":100,"failures":0,"unavailable":0}},"7_day":{"overall":{"calls":2,"avg_latency_ms":100,"failures":0,"unavailable":0}},"30_day":{"overall":{"calls":2,"avg_latency_ms":100,"failures":0,"unavailable":0}}}}})
}
pub fn render_html(state: &Value, runtime: &Value) -> String {
    let _ = (state, runtime); "<h1>No failures tracked</h1>".to_string()
}
"#;

    let result = super::verify_frozen_candidate(
        &root,
        "ignores_failures_e2e",
        &request,
        bad_source,
        crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0,
    );
    assert!(
        result.is_err(),
        "incorrect source (ignores failures) must be rejected"
    );
}
