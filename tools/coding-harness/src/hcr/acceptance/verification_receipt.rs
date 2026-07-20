//! VerificationReceipt — internal acceptance evidence for the external Harness.
//!
//! This struct captures the full acceptance bundle evidence inside the
//! Verification Harness. It is converted into an `ExternalReceiptEnvelope`
//! before being submitted to the Kernel.
//!
//! The Kernel never sees the fields of this struct. It only sees the
//! opaque `receipt_digest` (via `ExternalReceiptEnvelope`).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Full internal verification receipt produced by the Acceptance Kit.
///
/// Contains all bundle-specific evidence that the Kernel does not
/// need to parse. The `receipt_digest` binds this receipt to the
/// corresponding `ExternalReceiptEnvelope`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReceipt {
    /// The acceptance bundle reference (e.g. "token-dashboard-v0").
    pub acceptance_bundle_ref: String,
    /// Digest of the acceptance bundle (source + fixtures + engine + runtime).
    pub acceptance_bundle_digest: String,
    /// Digest of the public specification shown to the model.
    pub public_spec_digest: String,
    /// Digest of the verifier engine used.
    pub verifier_engine_digest: String,
    /// Per-file digests of fixtures used during verification.
    pub fixture_digests: Vec<String>,
    /// Version of the verifier runtime that executed the verification.
    pub verifier_runtime_version: String,
}

impl VerificationReceipt {
    /// Compute the opaque digest for this receipt.
    /// This digest is what the Kernel stores as `opaque_payload_digest`.
    pub fn opaque_digest(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        format!("sha256:{}", hex::encode(Sha256::digest(&canonical)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_receipt_has_deterministic_digest() {
        let receipt = VerificationReceipt {
            acceptance_bundle_ref: "token-dashboard-v0".into(),
            acceptance_bundle_digest: "bundle_sha256:abc123".into(),
            public_spec_digest: "sha256:def456".into(),
            verifier_engine_digest: "sha256:ghi789".into(),
            fixture_digests: vec!["sha256:aaa".into()],
            verifier_runtime_version: "0.1.0".into(),
        };
        let digest = receipt.opaque_digest();
        assert!(digest.starts_with("sha256:"));
        assert_eq!(digest.len(), 71); // "sha256:" + 64 hex chars

        // Same data → same digest
        let receipt2 = receipt.clone();
        assert_eq!(receipt.opaque_digest(), receipt2.opaque_digest());
    }
}
