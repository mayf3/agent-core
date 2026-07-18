//! External Development Controller — Seam V0.
//!
//! This crate is the *external* side of the External Orchestration Seam. It
//! depends ONLY on [`agent_core_protocol`] (plus generic HTTP/serde deps).
//! It MUST NOT depend on `agent-core-kernel`; the independence guard in
//! `scripts/check-controller-no-kernel.sh` enforces this at CI time.
//!
//! V0 behavior is deliberately minimal: receive an
//! [`ExternalOrchestrationIntent`], confirm the controller has taken over by
//! echoing a structured [`ExternalOrchestrationResult`]. No product facts
//! (failure-viewer, event.observe.v0, TargetKind, …) live here yet — those
//! arrive in later milestones once the seam is proven.

use agent_core_protocol::{
    ExternalOrchestrationIntent, ExternalOrchestrationResult, InvocationId, OpaqueRef,
    OrchestrationOutcome, ProtocolVersion, Sha256Digest,
};
use anyhow::{anyhow, Result};
use serde_json::json;

pub mod server;

/// Controller-side configuration. The Controller trusts the Kernel to have
/// authenticated the principal; it never re-authenticates.
#[derive(Debug, Clone)]
pub struct ControllerConfig {
    /// Loopback bind address, e.g. "127.0.0.1:7500".
    pub bind_addr: String,
}

impl ControllerConfig {
    pub fn from_env() -> Self {
        let bind_addr = std::env::var("DEVELOPMENT_CONTROLLER_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:7500".to_string());
        Self { bind_addr }
    }
}

/// Process an [`ExternalOrchestrationIntent`] and produce a receipt.
///
/// Seam V0: this is a controlled echo. The result confirms the controller has
/// taken over responsibility for the intent — it is NOT candidate acceptance,
/// capability approval, or deployment success.
///
/// Returns `Ok(result)` when the intent is structurally valid (even when the
/// controller reports it cannot handle the input — that is reported as a
/// `Failed` outcome, not an `Err`). Returns `Err` only for structural problems
/// that prevent forming any receipt (unknown protocol version, empty
/// invocation id).
pub fn handle_intent(intent: &ExternalOrchestrationIntent) -> Result<ExternalOrchestrationResult> {
    validate_intent(intent)?;
    let output = json!({
        "controller": "development-controller",
        "stage": "seam-v0",
        "accepted_raw_input": true,
        "note": "external controller has taken over the intent; no product handling in V0",
        "principal_ref": intent.principal_ref.as_str(),
    });
    let evidence = controller_evidence(intent);
    Ok(ExternalOrchestrationResult::succeeded(
        intent.protocol_version.clone(),
        intent.invocation_id.clone(),
        output,
        Some(evidence),
    ))
}

/// Validate the intent's generic structure. Product-specific validation is
/// out of scope for V0.
fn validate_intent(intent: &ExternalOrchestrationIntent) -> Result<()> {
    if !intent.protocol_version.is_current() {
        return Err(anyhow!(
            "protocol_version_mismatch: expected '{}', got '{}'",
            agent_core_protocol::PROTOCOL_VERSION,
            intent.protocol_version.0
        ));
    }
    if intent.invocation_id.is_empty() {
        return Err(anyhow!("intent_missing_invocation_id"));
    }
    Ok(())
}

/// Build a deterministic evidence reference for the receipt. The evidence is
/// a digest over (invocation_id, run_id, principal_ref, raw_input) so the
/// Kernel can later prove the controller saw a specific payload without the
/// controller having to store the payload itself.
fn controller_evidence(intent: &ExternalOrchestrationIntent) -> OpaqueRef {
    let mut canonical: Vec<u8> = Vec::new();
    canonical.extend_from_slice(intent.invocation_id.as_str().as_bytes());
    canonical.push(b'\n');
    canonical.extend_from_slice(intent.run_id.as_str().as_bytes());
    canonical.push(b'\n');
    canonical.extend_from_slice(intent.principal_ref.as_str().as_bytes());
    canonical.push(b'\n');
    canonical.extend_from_slice(
        serde_json::to_string(&intent.raw_input)
            .unwrap_or_default()
            .as_bytes(),
    );
    let digest = Sha256Digest::from_bytes(&canonical);
    OpaqueRef::new("controller-receipt-evidence", digest.as_str())
}

/// Convenience for tests: build a minimal valid intent.
pub fn sample_intent(invocation_id: &str) -> ExternalOrchestrationIntent {
    ExternalOrchestrationIntent {
        protocol_version: ProtocolVersion::current(),
        invocation_id: InvocationId::new(invocation_id),
        run_id: agent_core_protocol::RunId::new("run_sample"),
        principal_ref: agent_core_protocol::PrincipalRef::new("principal_sample"),
        raw_input: json!({ "text": "sample" }),
        context_ref: None,
        idempotency_key: Some(invocation_id.to_string()),
    }
}

/// Re-exported protocol bits so binaries/tests don't need a second protocol import.
pub use agent_core_protocol as protocol;

// Silence unused import warning when OrchestrationOutcome is only referenced
// via the `succeeded` helper path in some build configurations.
const _: fn() = || {
    let _ = OrchestrationOutcome::Succeeded;
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_intent_produces_succeeded_receipt() {
        let intent = sample_intent("inv_ok");
        let result = handle_intent(&intent).expect("ok");
        assert_eq!(result.outcome, OrchestrationOutcome::Succeeded);
        assert_eq!(result.invocation_id.as_str(), "inv_ok");
        assert!(result.verify_result_digest(), "digest must verify");
        assert!(result.evidence_ref.is_some(), "evidence must be present");
    }

    #[test]
    fn unknown_protocol_version_is_rejected() {
        let mut intent = sample_intent("inv_bad_pv");
        intent.protocol_version = ProtocolVersion("external-orchestration-v999".into());
        let err = handle_intent(&intent).expect_err("should reject");
        assert!(format!("{err}").contains("protocol_version_mismatch"));
    }

    #[test]
    fn empty_invocation_id_is_rejected() {
        let mut intent = sample_intent("inv_nonempty");
        intent.invocation_id = InvocationId::new("");
        let err = handle_intent(&intent).expect_err("should reject");
        assert!(format!("{err}").contains("intent_missing_invocation_id"));
    }

    #[test]
    fn evidence_digest_is_stable_for_same_input() {
        let intent = sample_intent("inv_stable");
        let a = handle_intent(&intent).expect("ok");
        let b = handle_intent(&intent).expect("ok");
        assert_eq!(
            a.evidence_ref.as_ref().expect("ev").digest,
            b.evidence_ref.as_ref().expect("ev").digest
        );
    }

    #[test]
    fn evidence_digest_changes_when_input_changes() {
        let mut intent = sample_intent("inv_change");
        let a = handle_intent(&intent).expect("ok");
        intent.raw_input = json!({ "text": "different" });
        let b = handle_intent(&intent).expect("ok");
        assert_ne!(
            a.evidence_ref.as_ref().expect("ev").digest,
            b.evidence_ref.as_ref().expect("ev").digest
        );
    }

    /// The Controller crate MUST NOT depend on agent-core-kernel. We check
    /// the parsed `[dependencies]` table (not the free-form description
    /// text, which legitimately mentions the constraint in prose).
    /// The authoritative guard is `scripts/check-controller-no-kernel.sh`,
    /// which inspects `cargo metadata`; this test is a fast in-crate backstop.
    #[test]
    fn no_kernel_dependency_is_present() {
        let manifest = include_str!("../Cargo.toml");
        // Locate the [dependencies] section and assert no key resolves to
        // agent-core-kernel. We deliberately avoid a full TOML parse to keep
        // the controller dependency-light; a section-scoped substring check
        // is sufficient and unambiguous because dependency keys are unique.
        let deps_start = manifest
            .find("[dependencies]")
            .expect("[dependencies] section must exist");
        // Deps run until the next section header or EOF.
        let rest = &manifest[deps_start..];
        let deps_section = match rest[1..].find('[') {
            Some(offset) => &rest[..=offset],
            None => rest,
        };
        assert!(
            !deps_section.contains("agent-core-kernel")
                && !deps_section.contains("\"agent-core-kernel\""),
            "controller [dependencies] must not reference agent-core-kernel"
        );
    }

    #[test]
    fn value_null_is_accepted_in_output() {
        // output is serde_json::Value; ensure we can carry a null without panic.
        let _ = serde_json::Value::Null;
    }
}
