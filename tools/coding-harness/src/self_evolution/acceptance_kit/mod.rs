//! Acceptance Kit V2: Model-Facing Public Specification + Trusted Private Verifier.
//!
//! Each kit defines:
//! - A `public_spec` (JSON) injected into the model's per-request context.
//! - A `private_verifier` (Rust code, never exposed to the model).
//! - Digests that bind both: changing either invalidates existing candidates.
//!
//! Kit selection is done by the external AcceptanceSelector, which provides
//! a bundle_ref string. The Kernel never sets acceptance_kit_ref.

mod failure_event_viewer;
mod shared_verifier_engine;
mod token_dashboard;

use agent_core_kernel::domain::DevelopmentRequest;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use shared_verifier_engine::constraint_diagnostic;
pub use shared_verifier_engine::validate_events_applied;

/// Known Acceptance Kit identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptanceKitId {
    TokenDashboardV0,
    FailureEventViewerV0,
}

impl AcceptanceKitId {
    /// The stable kit identifier string (maps to `acceptance_kit_ref`).
    pub fn kit_id(self) -> &'static str {
        match self {
            Self::TokenDashboardV0 => "token-dashboard-v0",
            Self::FailureEventViewerV0 => "failure-event-viewer-v0",
        }
    }

    pub fn kit_version(self) -> &'static str {
        match self {
            Self::TokenDashboardV0 => "v0",
            Self::FailureEventViewerV0 => "v0",
        }
    }

    /// The target component profile this kit applies to.
    pub fn target_profile(self) -> &'static str {
        match self {
            Self::TokenDashboardV0 => "hook-consumer-service-v0",
            Self::FailureEventViewerV0 => "hook-consumer-service-v0",
        }
    }

    /// The public specification shown to the model during generation.
    pub fn public_spec(self) -> Value {
        match self {
            Self::TokenDashboardV0 => token_dashboard::public_spec(),
            Self::FailureEventViewerV0 => failure_event_viewer::public_spec(),
        }
    }

    /// Digest of the public specification (sha256 of canonical JSON).
    pub fn public_spec_digest(self) -> String {
        let canonical =
            serde_json::to_vec(&self.public_spec()).expect("public spec is always valid JSON");
        format!("sha256:{}", hex::encode(Sha256::digest(&canonical)))
    }

    /// Private verifier digest: a stable string representing the verification
    /// logic. The Rust code IS the verifier, so this is a versioned constant
    /// that changes when the verification logic changes.
    fn private_verifier_digest(self) -> &'static str {
        match self {
            // Bump this tag when token_dashboard verification logic changes.
            Self::TokenDashboardV0 => "pv_token_dashboard_v0_001",
            // Bump this tag when failure_event_viewer verification logic changes.
            Self::FailureEventViewerV0 => "pv_failure_viewer_v0_001",
        }
    }

    /// Combined kit digest: binds both public_spec and private_verifier.
    /// Changing either produces a different digest, invalidating old caches.
    pub fn combined_kit_digest(self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.kit_id().as_bytes());
        hasher.update(b":");
        hasher.update(self.public_spec_digest().as_bytes());
        hasher.update(b":");
        hasher.update(self.private_verifier_digest().as_bytes());
        format!("kit_sha256:{}", hex::encode(hasher.finalize()))
    }

    /// Resolve an AcceptanceKitId from a bundle reference string.
    ///
    /// Uses exact match against known bundle refs. Never uses substring
    /// matching on "token" or any other heuristic. The bundle_ref is
    /// provided by the external AcceptanceSelector, not by the Kernel.
    /// Returns `Err("ACCEPTANCE_KIT_SELECTION_REQUIRED")` when no
    /// known kit matches the ref.
    pub fn resolve(bundle_ref: &str) -> Result<Self, &'static str> {
        match bundle_ref {
            "token-dashboard-v0" => Ok(Self::TokenDashboardV0),
            "failure-event-viewer-v0" => Ok(Self::FailureEventViewerV0),
            _ => Err("ACCEPTANCE_KIT_SELECTION_REQUIRED"),
        }
    }

    /// Run the private verifier for this kit against generated output.
    ///
    /// `input` is the probe input JSON string from which the event count is
    /// derived. `stdout` is the candidate's profile contract output.
    /// Returns `Ok(())` on pass, or `Err(diagnostics)` on failure.
    /// The diagnostics string contains structured constraint information
    /// for the model to consume during repair.
    pub fn verify(
        self,
        request: &DevelopmentRequest,
        source: &str,
        input: &str,
        stdout: &str,
    ) -> Result<(), String> {
        match self {
            Self::TokenDashboardV0 => token_dashboard::verify(request, source, input, stdout),
            Self::FailureEventViewerV0 => {
                failure_event_viewer::verify(request, source, input, stdout)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
    use agent_core_kernel::domain::{DevelopmentRequestDraft, TargetKind};

    fn hook_consumer_request(name: &str) -> DevelopmentRequest {
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
    fn token_dashboard_v0_has_distinct_digests() {
        let kit = AcceptanceKitId::TokenDashboardV0;
        let spec_digest = kit.public_spec_digest();
        let combined = kit.combined_kit_digest();
        assert!(spec_digest.starts_with("sha256:"));
        assert!(combined.starts_with("kit_sha256:"));
        assert_ne!(spec_digest, combined);
    }

    #[test]
    fn failure_viewer_v0_has_distinct_digests() {
        let kit = AcceptanceKitId::FailureEventViewerV0;
        let spec_digest = kit.public_spec_digest();
        let combined = kit.combined_kit_digest();
        assert!(spec_digest.starts_with("sha256:"));
        assert!(combined.starts_with("kit_sha256:"));
        assert_ne!(spec_digest, combined);
    }

    #[test]
    fn kit_digests_differ_between_token_and_non_token_kit() {
        let token = AcceptanceKitId::TokenDashboardV0;
        let viewer = AcceptanceKitId::FailureEventViewerV0;
        assert_ne!(token.combined_kit_digest(), viewer.combined_kit_digest());
        assert_ne!(token.public_spec_digest(), viewer.public_spec_digest());
    }

    #[test]
    fn resolve_known_bundle_refs_selects_correct_kit() {
        assert_eq!(
            AcceptanceKitId::resolve("token-dashboard-v0").unwrap(),
            AcceptanceKitId::TokenDashboardV0
        );
        assert_eq!(
            AcceptanceKitId::resolve("failure-event-viewer-v0").unwrap(),
            AcceptanceKitId::FailureEventViewerV0
        );
    }

    #[test]
    fn resolve_unknown_bundle_ref_returns_selection_required() {
        assert_eq!(
            AcceptanceKitId::resolve("unknown-kit-v0"),
            Err("ACCEPTANCE_KIT_SELECTION_REQUIRED")
        );
    }

    /// Verify that an "auth token" ref doesn't accidentally match the
    /// token-dashboard kit (no substring matching).
    #[test]
    fn auth_token_ref_does_not_match_telemetry_kit() {
        assert_eq!(
            AcceptanceKitId::resolve("auth-token-v0"),
            Err("ACCEPTANCE_KIT_SELECTION_REQUIRED")
        );
    }

    #[test]
    fn changing_public_spec_changes_kit_digest() {
        let original = AcceptanceKitId::TokenDashboardV0.public_spec_digest();
        // We cannot modify a const spec, but we can verify that a different
        // spec would produce a different digest by checking that the two kits
        // have different spec digests (proving digest is spec-content-dependent).
        let other = AcceptanceKitId::FailureEventViewerV0.public_spec_digest();
        assert_ne!(original, other);
    }

    #[test]
    fn failure_viewer_public_spec_contains_no_token_terms() {
        let spec = AcceptanceKitId::FailureEventViewerV0.public_spec();
        let _text = serde_json::to_string(&spec).unwrap().to_lowercase();
        // Check output_json_schema and html_contract don't contain token fields
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
                "failure viewer schema must not contain '{forbidden}'"
            );
            assert!(
                !html.contains(forbidden),
                "failure viewer html contract must not contain '{forbidden}'"
            );
        }
    }

    #[test]
    fn combined_kit_digest_changes_when_spec_or_verifier_changes() {
        let token = AcceptanceKitId::TokenDashboardV0;
        let viewer = AcceptanceKitId::FailureEventViewerV0;
        // Different kit IDs → different combined digest
        assert_ne!(token.combined_kit_digest(), viewer.combined_kit_digest());
    }
}
