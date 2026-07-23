//! Acceptance Kit dispatch for hook consumer service generation.
//!
//! This module provides the shared profile contract checker used by
//! all Acceptance Kits. Individual kit private verifiers live in
//! `crate::self_evolution::acceptance_kit::*`.
//!
//! Kit selection is done externally via the AcceptanceSelector and
//! the selected bundle_ref is passed in as a parameter. No substring
//! matching on "token" is used anywhere in this module.

use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::{json, Value};

/// Validate the profile contract output (shared check for all kits).
///
/// `input` is the probe input JSON string whose `events` array is used
/// to determine the expected `events_applied` count. This ensures the
/// check works with any event count rather than a hardcoded magic number.
///
/// The remaining profile fields (ok, schema_version, html_*) are checked
/// with fixed expected values.
pub(super) fn validate_profile_contract(input: &str, stdout: &str) -> Result<(), String> {
    // Events-applied validation (count comes from actual input)
    crate::self_evolution::acceptance_kit::validate_events_applied(input, stdout)?;

    // Remaining profile fields (fixed expected values)
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

/// Validate contracts against a specific Acceptance Kit.
///
/// Resolves the correct Acceptance Kit from the bundle_ref, runs the
/// profile contract, then the kit's private verifier. If bundle_ref
/// is unknown returns `ACCEPTANCE_KIT_SELECTION_REQUIRED`.
pub(super) fn validate_contracts(
    bundle_ref: &str,
    request: &DevelopmentRequest,
    input: &str,
    stdout: &str,
) -> Result<(), String> {
    let kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::resolve(bundle_ref)
        .map_err(|_| format!("ACCEPTANCE_KIT_SELECTION_REQUIRED: bundle_ref '{bundle_ref}' is unknown. The external AcceptanceSelector must set a valid bundle_ref."))?;

    // Profile contract is shared across all hook consumer kits.
    validate_profile_contract(input, stdout)?;

    // Kit-specific verification does NOT have access to the source here
    // (source policy check runs earlier in compile_probe). The verify
    // method handles request contract validation only.
    kit.verify(request, "", input, stdout)
}

/// Validate the generated source against kit-specific source policies.
///
/// This is called separately in compile_probe before compiling, so it
/// receives the source string directly.
pub(super) fn validate_source(bundle_ref: &str, source: &str) -> Result<(), String> {
    let kit = crate::self_evolution::acceptance_kit::AcceptanceKitId::resolve(bundle_ref)
        .map_err(|_| "ACCEPTANCE_KIT_SELECTION_REQUIRED".to_string())?;

    // The Token Dashboard kit has source-level policies (no within_days
    // in apply_event, no today_utc). Other kits may opt out.
    match kit {
        crate::self_evolution::acceptance_kit::AcceptanceKitId::TokenDashboardV0 => {
            validate_token_source(source)
        }
        crate::self_evolution::acceptance_kit::AcceptanceKitId::FailureEventViewerV0 => {
            // Failure Event Viewer has no additional source policies.
            Ok(())
        }
        crate::self_evolution::acceptance_kit::AcceptanceKitId::FailureViewerQueryV0 => {
            Err("ACCEPTANCE_KIT_PROFILE_MISMATCH".into())
        }
    }
}

/// Token Dashboard source policy: no within_days in apply_event, no today_utc().
fn validate_token_source(source: &str) -> Result<(), String> {
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

pub(super) fn truncate_diagnostics(value: &str) -> String {
    let mut end = value.len().min(16 * 1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
