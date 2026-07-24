//! External Acceptance Bundle Selector.
//!
//! This is a temporary deterministic selector that maps component names
//! to acceptance bundle references. It can be replaced by a future
//! multi-Agent Planner that produces the same `AcceptanceSelection` struct.
//!
//! Selection rules:
//! - Uses exact component name matching only.
//! - Never uses substring matching on "token" or any other heuristic.
//! - Unknown requests return ACCEPTANCE_KIT_SELECTION_REQUIRED.
//! - The Kernel never sees the selection logic.

use agent_core_kernel::domain::DevelopmentRequest;

/// The result of selecting an acceptance bundle for a DevelopmentRequest.
#[derive(Debug, Clone)]
pub struct AcceptanceSelection {
    /// The acceptance bundle reference (e.g. "token-dashboard-v0").
    pub bundle_ref: String,
    /// Digest of the selected acceptance bundle.
    /// Computed at build time from the bundle's source files, fixtures,
    /// shared engine, and verifier runtime version.
    pub bundle_digest: String,
}

impl AcceptanceSelection {
    pub fn new(bundle_ref: &str, bundle_digest: &str) -> Self {
        Self {
            bundle_ref: bundle_ref.to_string(),
            bundle_digest: bundle_digest.to_string(),
        }
    }
}

/// Select an acceptance bundle for the given DevelopmentRequest.
///
/// The selection is based purely on the component name extracted by the
/// router. No heuristic, AI, or substring matching is used.
pub fn select(request: &DevelopmentRequest) -> Result<AcceptanceSelection, String> {
    match request.name.as_str() {
        "token-dashboard" => Ok(AcceptanceSelection::new(
            "token-dashboard-v0",
            acceptance_bundle_digest("token-dashboard-v0"),
        )),
        "failure-viewer" | "failure-event-viewer" => Ok(AcceptanceSelection::new(
            "failure-event-viewer-v0",
            acceptance_bundle_digest("failure-event-viewer-v0"),
        )),
        "external.failure_viewer_query" => Ok(AcceptanceSelection::new(
            "failure-viewer-query-v0",
            acceptance_bundle_digest("failure-viewer-query-v0"),
        )),
        _ => Err("ACCEPTANCE_KIT_SELECTION_REQUIRED".to_string()),
    }
}

/// Look up the build-time bundle digest for a given bundle ref.
///
/// These environment variables are set by build.rs and contain the
/// SHA-256 of each bundle's canonical manifest (source files, fixtures,
/// shared engine, verifier runtime version, Cargo features).
fn acceptance_bundle_digest(bundle_ref: &str) -> &'static str {
    match bundle_ref {
        "token-dashboard-v0" => env!("TOKEN_DASHBOARD_BUNDLE_DIGEST"),
        "failure-event-viewer-v0" => env!("FAILURE_VIEWER_BUNDLE_DIGEST"),
        "failure-viewer-query-v0" => env!("FAILURE_VIEWER_QUERY_BUNDLE_DIGEST"),
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
    use agent_core_kernel::domain::{DevelopmentRequestDraft, TargetKind};

    fn request(name: &str) -> DevelopmentRequest {
        let mut draft = DevelopmentRequestDraft::new(TargetKind::HookConsumerService, name.into());
        draft.requirements = vec!["test requirement".into()];
        draft.required_contracts = vec!["event.observe.v0".into()];
        draft.requested_permissions = vec!["journal.observe".into()];
        draft.acceptance_criteria = vec!["test criteria".into()];
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

    #[test]
    fn select_token_dashboard() {
        let req = request("token-dashboard");
        let selection = select(&req).unwrap();
        assert_eq!(selection.bundle_ref, "token-dashboard-v0");
        assert!(!selection.bundle_digest.is_empty());
    }

    #[test]
    fn select_failure_viewer() {
        let req = request("failure-viewer");
        let selection = select(&req).unwrap();
        assert_eq!(selection.bundle_ref, "failure-event-viewer-v0");
    }

    #[test]
    fn select_unknown_returns_selection_required() {
        let req = request("unknown-component");
        let err = select(&req).unwrap_err();
        assert!(err.contains("ACCEPTANCE_KIT_SELECTION_REQUIRED"));
    }

    #[test]
    fn no_substring_matching_on_token() {
        let req = request("auth-token-manager");
        let err = select(&req).unwrap_err();
        assert!(err.contains("ACCEPTANCE_KIT_SELECTION_REQUIRED"));
    }
}
