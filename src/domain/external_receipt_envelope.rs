//! Generic external receipt envelope understood by the Kernel.
//!
//! The external Verification Harness returns this envelope after
//! completing verification. The Kernel validates it mechanically:
//!
//! - `invocation_intent_id`: matches the Kernel's issued InvocationIntent
//! - `issuer`: authenticated via channel trust (mTLS / control token / loopback)
//! - `subject_digest`: matches the candidate or artifact being verified
//! - `outcome`: Passed or Failed
//! - `evidence_digest`: SHA-256 of the internal evidence
//! - `opaque_payload_digest`: SHA-256 of bundle-specific details (Kernel does not parse)
//! - `receipt_digest`: SHA-256 of all above fields — proves content integrity, NOT origin

use serde::{Deserialize, Serialize};

/// Outcome of external verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalOutcome {
    Passed,
    Failed,
}

/// The generic receipt envelope that the Kernel receives from the external
/// Verification Harness. The Kernel validates this envelope without parsing
/// any Acceptance Kit, Bundle, Spec, or Verifier semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalReceiptEnvelope {
    /// The InvocationIntent ID that this receipt responds to.
    /// Used by the Kernel to prevent receipt misassignment.
    pub invocation_intent_id: String,
    /// Identity of the receipt issuer (Verification Harness).
    /// Trust is established via channel authentication (mTLS, control token,
    /// or local loopback) combined with the InvocationIntent binding.
    /// NOT a self-signed claim — SHA-256 alone cannot prove origin.
    pub issuer: String,
    /// Digest of the subject (candidate or artifact) that was verified.
    /// The Kernel checks that this matches the artifact being deployed.
    pub subject_digest: String,
    /// Verification outcome.
    pub outcome: ExternalOutcome,
    /// Digest of the evidence backing this receipt.
    pub evidence_digest: String,
    /// Optional digest of the opaque payload (bundle-specific details).
    /// The Kernel stores this digest but does NOT parse the payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque_payload_digest: Option<String>,
    /// SHA-256 of all preceding fields in canonical order.
    /// This proves content integrity: any change to the receipt fields
    /// produces a different digest. Origin authentication is handled
    /// via the invocation binding and channel trust, not by this hash.
    pub receipt_digest: String,
}

impl ExternalReceiptEnvelope {
    /// Validate the receipt envelope's structural integrity.
    /// Does NOT verify the issuer's identity (that is done via channel trust).
    pub fn validate_structure(&self) -> Result<(), &'static str> {
        if self.invocation_intent_id.is_empty() {
            return Err("receipt missing invocation_intent_id");
        }
        if self.issuer.is_empty() {
            return Err("receipt missing issuer");
        }
        if self.subject_digest.is_empty() || !self.subject_digest.starts_with("sha256:") {
            return Err("receipt missing or invalid subject_digest");
        }
        if self.evidence_digest.is_empty() || !self.evidence_digest.starts_with("sha256:") {
            return Err("receipt missing or invalid evidence_digest");
        }
        if self.receipt_digest.is_empty() {
            return Err("receipt missing receipt_digest");
        }
        Ok(())
    }
}
