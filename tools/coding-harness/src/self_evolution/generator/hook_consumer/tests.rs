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
