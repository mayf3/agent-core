//! The two DTOs that cross the External Orchestration Seam: the Kernel sends
//! an [`ExternalOrchestrationIntent`]; the Controller replies with an
//! [`ExternalOrchestrationResult`].
//!
//! Neither DTO carries any product-specific fact. The Kernel does not know
//! what the Controller will do with `raw_input`, and the Controller does not
//! know how the Kernel will record the receipt.

use crate::digest::compute_result_digest;
use crate::refs::{InvocationId, OpaqueRef, PrincipalRef, RunId};
use crate::version::ProtocolVersion;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the Kernel sends to the external Development Controller.
///
/// The Kernel fills `principal_ref`, `run_id`, and `invocation_id` from the
/// governance context it has already established; `raw_input` is the
/// user-facing payload forwarded verbatim (the Kernel does NOT interpret it);
/// `context_ref` optionally points at externally-stored context the Controller
/// may need; `idempotency_key` lets the Controller deduplicate retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalOrchestrationIntent {
    pub protocol_version: ProtocolVersion,
    pub invocation_id: InvocationId,
    pub run_id: RunId,
    pub principal_ref: PrincipalRef,
    /// Forwarded user input. Seam V0 places no structure on this.
    pub raw_input: Value,
    /// Optional opaque reference to externally-staged context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_ref: Option<OpaqueRef>,
    /// Dedup key. The Kernel sets this; the Controller should treat equal
    /// `(invocation_id, idempotency_key)` as the same logical call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Outcome of an external orchestration call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationOutcome {
    /// The Controller accepted and processed the intent.
    Succeeded,
    /// The Controller explicitly rejected the intent (e.g. unsupported input).
    Failed,
}

impl OrchestrationOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrchestrationOutcome::Succeeded => "succeeded",
            OrchestrationOutcome::Failed => "failed",
        }
    }
}

/// What the Controller returns to the Kernel. This is a **call receipt only**.
///
/// Semantics (frozen by Seam V0 / docs/decisions/external-orchestration-seam-v0.md):
/// an `ExternalOrchestrationResult` records that *one approved external
/// invocation returned a verifiable result*. It is NOT candidate acceptance,
/// capability approval, deployment success, or a registry effect. Recording
/// this receipt in the Kernel triggers NO Proposal, NO Deployment, and NO
/// Registry mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalOrchestrationResult {
    pub protocol_version: ProtocolVersion,
    /// MUST equal the `invocation_id` of the intent. The Kernel rejects a
    /// mismatch.
    pub invocation_id: InvocationId,
    pub outcome: OrchestrationOutcome,
    /// Free-form Controller payload. The Kernel stores but does not interpret it.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub output: Value,
    /// Optional opaque evidence reference. When present, its digest
    /// participates in `result_digest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<OpaqueRef>,
    /// Canonical `sha256:<hex>` over (protocol_version, invocation_id,
    /// outcome, evidence_ref digest). Recomputed and checked by the Kernel.
    pub result_digest: String,
}

impl ExternalOrchestrationResult {
    /// Recompute the canonical result digest for this result.
    pub fn recompute_result_digest(&self) -> String {
        compute_result_digest(
            &self.protocol_version.0,
            self.invocation_id.as_str(),
            self.outcome.as_str(),
            self.evidence_ref.as_ref().map(|r| r.digest.as_str()),
        )
    }

    /// True iff the stored `result_digest` matches the independently
    /// recomputed value.
    pub fn verify_result_digest(&self) -> bool {
        self.result_digest == self.recompute_result_digest()
    }

    /// Build a succeeded result, computing the correct digest.
    pub fn succeeded(
        protocol_version: ProtocolVersion,
        invocation_id: InvocationId,
        output: Value,
        evidence_ref: Option<OpaqueRef>,
    ) -> Self {
        let outcome = OrchestrationOutcome::Succeeded;
        let result_digest = compute_result_digest(
            &protocol_version.0,
            invocation_id.as_str(),
            outcome.as_str(),
            evidence_ref.as_ref().map(|r| r.digest.as_str()),
        );
        Self {
            protocol_version,
            invocation_id,
            outcome,
            output,
            evidence_ref,
            result_digest,
        }
    }

    /// Build a failed result, computing the correct digest.
    pub fn failed(
        protocol_version: ProtocolVersion,
        invocation_id: InvocationId,
        output: Value,
        evidence_ref: Option<OpaqueRef>,
    ) -> Self {
        let outcome = OrchestrationOutcome::Failed;
        let result_digest = compute_result_digest(
            &protocol_version.0,
            invocation_id.as_str(),
            outcome.as_str(),
            evidence_ref.as_ref().map(|r| r.digest.as_str()),
        );
        Self {
            protocol_version,
            invocation_id,
            outcome,
            output,
            evidence_ref,
            result_digest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::PROTOCOL_VERSION;

    fn pv() -> ProtocolVersion {
        ProtocolVersion::current()
    }

    #[test]
    fn succeeded_result_has_valid_digest() {
        let r = ExternalOrchestrationResult::succeeded(
            pv(),
            InvocationId::new("inv_1"),
            serde_json::json!({ "echo": "ok" }),
            None,
        );
        assert!(r.verify_result_digest());
        assert_eq!(r.outcome, OrchestrationOutcome::Succeeded);
    }

    #[test]
    fn tampered_digest_is_detected() {
        let mut r = ExternalOrchestrationResult::succeeded(
            pv(),
            InvocationId::new("inv_1"),
            serde_json::json!({ "echo": "ok" }),
            None,
        );
        r.result_digest = "sha256:deadbeef".to_string();
        assert!(!r.verify_result_digest());
    }

    #[test]
    fn changing_invocation_id_invalidates_stored_digest() {
        let r = ExternalOrchestrationResult::succeeded(
            pv(),
            InvocationId::new("inv_1"),
            serde_json::json!({ "echo": "ok" }),
            None,
        );
        let mut tampered = r.clone();
        tampered.invocation_id = InvocationId::new("inv_2");
        // Stored digest was computed for inv_1; it no longer matches.
        assert_ne!(tampered.result_digest, tampered.recompute_result_digest());
    }

    #[test]
    fn outcome_serializes_as_snake_case() {
        let s = serde_json::to_string(&OrchestrationOutcome::Succeeded).expect("serialize");
        let f = serde_json::to_string(&OrchestrationOutcome::Failed).expect("serialize");
        assert_eq!(s, "\"succeeded\"");
        assert_eq!(f, "\"failed\"");
    }

    #[test]
    fn intent_round_trips_serde() {
        let intent = ExternalOrchestrationIntent {
            protocol_version: pv(),
            invocation_id: InvocationId::new("inv_42"),
            run_id: RunId::new("run_42"),
            principal_ref: PrincipalRef::new("feishu:ou_abc"),
            raw_input: serde_json::json!({ "text": "hello" }),
            context_ref: Some(OpaqueRef::new("context", "sha256:cccc")),
            idempotency_key: Some("key_42".into()),
        };
        let json = serde_json::to_string(&intent).expect("serialize");
        let back: ExternalOrchestrationIntent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.invocation_id, intent.invocation_id);
        assert_eq!(back.idempotency_key, Some("key_42".to_string()));
    }

    #[test]
    fn protocol_version_field_is_validated_by_kernel_not_crate() {
        // The crate does not reject an unknown protocol_version at parse time;
        // the Kernel's seam layer enforces `is_current()`. Here we only assert
        // the current constant is what the helper produces.
        assert_eq!(pv().0, PROTOCOL_VERSION);
    }
}
