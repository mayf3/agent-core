//! Acceptance Kit dispatch for hook consumer service generation.
//!
//! This module provides the shared profile contract checker used by
//! all Acceptance Kits. Individual kit private verifiers live in
//! `crate::self_evolution::acceptance_kit::*`.
//!
//! Kit selection is explicit via `DevelopmentRequest.acceptance_kit_ref`.
//! No substring matching on "token" is used anywhere in this module.

use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};

/// Validate the profile contract output.
///
/// This is a shared check for all hook consumer service kits:
/// the generated binary's `--profile-contract-test` output must
/// contain standard profile fields.
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

/// Combine profile contract validation with Acceptance Kit verification.
///
/// Resolves the correct Acceptance Kit from the request, runs the profile
/// contract, then the kit's private verifier. If no kit can be resolved
/// returns `ACCEPTANCE_KIT_SELECTION_REQUIRED`.
pub(super) fn validate_contracts(request: &DevelopmentRequest, stdout: &str) -> Result<(), String> {
    let kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::resolve(request)
        .map_err(|_| "ACCEPTANCE_KIT_SELECTION_REQUIRED: no explicit acceptance_kit_ref in DevelopmentRequest and no default kit available. The routing layer must set acceptance_kit_ref for this component.".to_string())?;

    // Profile contract is shared across all hook consumer kits.
    validate_profile_contract(stdout)?;

    // Kit-specific verification does NOT have access to the source here
    // (source policy check runs earlier in compile_probe). The verify
    // method handles request contract validation only.
    kit.verify(request, "", stdout)
}

/// Validate the generated source against kit-specific source policies.
///
/// This is called separately in compile_probe before compiling, so it
/// receives the source string directly.
pub(super) fn validate_source(request: &DevelopmentRequest, source: &str) -> Result<(), String> {
    let kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::resolve(request)
        .map_err(|_| "ACCEPTANCE_KIT_SELECTION_REQUIRED".to_string())?;

    // The Token Dashboard kit has source-level policies (no within_days
    // in apply_event, no today_utc). Other kits may opt out.
    // We call the kit's verify which handles both request and source.
    // But since source and stdout need to be checked separately, we
    // have the kit implement source-only validation here.
    match kit {
        crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0 => {
            validate_token_source(source)
        }
        crate::self_evolution::acceptance_kit::AcceptanceKitId::FailureEventViewerV0 => {
            // Failure Event Viewer has no additional source policies.
            Ok(())
        }
    }
}

/// Token Dashboard source policy: no within_days in apply_event, no today_utc().
fn validate_token_source(source: &str) -> Result<(), String> {
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

pub(super) fn truncate_diagnostics(value: &str) -> String {
    let mut end = value.len().min(16 * 1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
