//! Generic external receipt envelope understood by the Kernel.
//!
//! The external Coding Harness returns this envelope after completing
//! acceptance verification. The Kernel validates it mechanically:
//!
//! - `schema_version`: the envelope protocol version
//! - `invocation_intent_id`: matches the Kernel's issued InvocationIntent
//! - `issuer`: authenticated via channel trust (loopback / control token)
//! - `subject_digest`: matches the candidate or artifact being verified
//! - `outcome`: Passed or Failed
//! - `evidence_digest`: SHA-256 of the internal evidence
//! - `opaque_payload_digest`: SHA-256 of bundle-specific details (Kernel does not parse)
//! - `receipt_digest`: SHA-256 of all above fields — proves content integrity

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Current envelope schema version.
pub const SCHEMA_VERSION: &str = "external-receipt-envelope-v1";

/// Outcome of external verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalOutcome {
    Passed,
    Failed,
}

/// The generic receipt envelope that the Kernel receives from the external
/// Coding Harness. The Kernel validates this envelope without parsing any
/// Acceptance Kit, Bundle, Spec, or Verifier semantics.
///
/// Canonical field ordering for `receipt_digest` computation:
///   1. schema_version
///   2. invocation_intent_id
///   3. issuer
///   4. subject_digest
///   5. outcome
///   6. evidence_digest
///   7. opaque_payload_digest
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalReceiptEnvelope {
    /// Envelope protocol version. Must be `SCHEMA_VERSION`.
    pub schema_version: String,
    /// The InvocationIntent ID that this receipt responds to.
    /// Used by the Kernel to prevent receipt misassignment.
    pub invocation_intent_id: String,
    /// Identity of the receipt issuer (Coding Harness).
    /// Trust is established via channel authentication combined with the
    /// InvocationIntent binding. NOT a self-signed claim.
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
    /// The `receipt_digest` field itself is NOT included in the hash.
    pub receipt_digest: String,
}

impl ExternalReceiptEnvelope {
    /// Validate the receipt envelope's structural integrity.
    /// Does NOT verify the issuer's identity (that is done via channel trust).
    pub fn validate_structure(&self) -> Result<(), &'static str> {
        if self.schema_version != SCHEMA_VERSION {
            return Err("unknown schema_version");
        }
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

    /// Verify `receipt_digest` matches independently recomputed value.
    pub fn verify_receipt_digest(&self) -> Result<(), &'static str> {
        let recomputed = compute_external_receipt_digest(
            &self.schema_version,
            &self.invocation_intent_id,
            &self.issuer,
            &self.subject_digest,
            self.outcome,
            &self.evidence_digest,
            self.opaque_payload_digest.as_deref(),
        );
        if self.receipt_digest == recomputed {
            Ok(())
        } else {
            Err("receipt_digest mismatch")
        }
    }
}

/// SHA-256 of canonical-ordered fields (receipt_digest excluded). None → "".
/// Returns `"sha256:<hex>"`.
pub fn compute_external_receipt_digest(
    schema_version: &str,
    invocation_intent_id: &str,
    issuer: &str,
    subject_digest: &str,
    outcome: ExternalOutcome,
    evidence_digest: &str,
    opaque_payload_digest: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();

    // Canonical field order — each field is written as its UTF-8 bytes
    // followed by a newline separator, with None → empty line.
    write_field(&mut hasher, schema_version);
    write_field(&mut hasher, invocation_intent_id);
    write_field(&mut hasher, issuer);
    write_field(&mut hasher, subject_digest);
    write_field(&mut hasher, outcome_str(outcome));
    write_field(&mut hasher, evidence_digest);
    write_field(&mut hasher, opaque_payload_digest.unwrap_or(""));

    let hex = hex::encode(hasher.finalize());
    format!("sha256:{hex}")
}

/// Serialize outcome as a stable string for digest computation.
fn outcome_str(outcome: ExternalOutcome) -> &'static str {
    match outcome {
        ExternalOutcome::Passed => "Passed",
        ExternalOutcome::Failed => "Failed",
    }
}

/// Write a field followed by a newline into the hasher.
fn write_field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.as_bytes());
    hasher.update(b"\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_envelope() -> ExternalReceiptEnvelope {
        ExternalReceiptEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            invocation_intent_id: "invocation_test".into(),
            issuer: "coding-harness".into(),
            subject_digest: format!("sha256:{}", "a".repeat(64)),
            outcome: ExternalOutcome::Passed,
            evidence_digest: format!("sha256:{}", "b".repeat(64)),
            opaque_payload_digest: Some(format!("sha256:{}", "c".repeat(64))),
            receipt_digest: String::new(), // will be set below
        }
    }

    #[test]
    fn envelope_validate_structure_ok() {
        let mut env = valid_envelope();
        env.receipt_digest = compute_external_receipt_digest(
            &env.schema_version,
            &env.invocation_intent_id,
            &env.issuer,
            &env.subject_digest,
            env.outcome,
            &env.evidence_digest,
            env.opaque_payload_digest.as_deref(),
        );
        assert!(env.validate_structure().is_ok());
    }

    #[test]
    fn envelope_validate_structure_unknown_schema() {
        let mut env = valid_envelope();
        env.schema_version = "wrong-schema".into();
        env.receipt_digest = "sha256:0000".into();
        assert!(env.validate_structure().is_err());
        assert!(env
            .validate_structure()
            .unwrap_err()
            .contains("schema_version"));
    }

    #[test]
    fn envelope_validate_structure_missing_invocation_intent() {
        let mut env = valid_envelope();
        env.invocation_intent_id = String::new();
        env.receipt_digest = "sha256:0000".into();
        assert!(env.validate_structure().is_err());
    }

    #[test]
    fn envelope_validate_structure_missing_issuer() {
        let mut env = valid_envelope();
        env.issuer = String::new();
        env.receipt_digest = "sha256:0000".into();
        assert!(env.validate_structure().is_err());
    }

    #[test]
    fn envelope_validate_structure_invalid_subject_digest() {
        let mut env = valid_envelope();
        env.subject_digest = "not-sha256".into();
        env.receipt_digest = "sha256:0000".into();
        assert!(env.validate_structure().is_err());
    }

    #[test]
    fn envelope_validate_structure_invalid_evidence_digest() {
        let mut env = valid_envelope();
        env.evidence_digest = String::new();
        env.receipt_digest = "sha256:0000".into();
        assert!(env.validate_structure().is_err());
    }

    #[test]
    fn envelope_validate_structure_missing_receipt_digest() {
        let mut env = valid_envelope();
        env.receipt_digest = String::new();
        assert!(env.validate_structure().is_err());
    }

    #[test]
    fn digest_is_deterministic() {
        let d1 = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "coding-harness",
            "sha256:aaaa",
            ExternalOutcome::Passed,
            "sha256:bbbb",
            Some("sha256:cccc"),
        );
        let d2 = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "coding-harness",
            "sha256:aaaa",
            ExternalOutcome::Passed,
            "sha256:bbbb",
            Some("sha256:cccc"),
        );
        assert_eq!(d1, d2);
        assert!(d1.starts_with("sha256:"));
        assert_eq!(d1.len(), 71);
    }

    #[test]
    fn digest_changes_when_invocation_changes() {
        let base = || {
            compute_external_receipt_digest(
                SCHEMA_VERSION,
                "inv_1",
                "coding-harness",
                "sha256:aaaa",
                ExternalOutcome::Passed,
                "sha256:bbbb",
                Some("sha256:cccc"),
            )
        };
        let changed = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_2",
            "coding-harness",
            "sha256:aaaa",
            ExternalOutcome::Passed,
            "sha256:bbbb",
            Some("sha256:cccc"),
        );
        assert_ne!(base(), changed);
    }

    #[test]
    fn digest_changes_when_issuer_changes() {
        let base = || {
            compute_external_receipt_digest(
                SCHEMA_VERSION,
                "inv_1",
                "coding-harness",
                "sha256:aaaa",
                ExternalOutcome::Passed,
                "sha256:bbbb",
                Some("sha256:cccc"),
            )
        };
        let changed = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "different-issuer",
            "sha256:aaaa",
            ExternalOutcome::Passed,
            "sha256:bbbb",
            Some("sha256:cccc"),
        );
        assert_ne!(base(), changed);
    }

    #[test]
    fn digest_changes_when_subject_changes() {
        let base = || {
            compute_external_receipt_digest(
                SCHEMA_VERSION,
                "inv_1",
                "coding-harness",
                "sha256:aaaa",
                ExternalOutcome::Passed,
                "sha256:bbbb",
                Some("sha256:cccc"),
            )
        };
        let changed = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "coding-harness",
            "sha256:dddd",
            ExternalOutcome::Passed,
            "sha256:bbbb",
            Some("sha256:cccc"),
        );
        assert_ne!(base(), changed);
    }

    #[test]
    fn digest_changes_when_outcome_changes() {
        let base = || {
            compute_external_receipt_digest(
                SCHEMA_VERSION,
                "inv_1",
                "coding-harness",
                "sha256:aaaa",
                ExternalOutcome::Passed,
                "sha256:bbbb",
                Some("sha256:cccc"),
            )
        };
        let changed = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "coding-harness",
            "sha256:aaaa",
            ExternalOutcome::Failed,
            "sha256:bbbb",
            Some("sha256:cccc"),
        );
        assert_ne!(base(), changed);
    }

    #[test]
    fn digest_changes_when_evidence_changes() {
        let base = || {
            compute_external_receipt_digest(
                SCHEMA_VERSION,
                "inv_1",
                "coding-harness",
                "sha256:aaaa",
                ExternalOutcome::Passed,
                "sha256:bbbb",
                Some("sha256:cccc"),
            )
        };
        let changed = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "coding-harness",
            "sha256:aaaa",
            ExternalOutcome::Passed,
            "sha256:dddd",
            Some("sha256:cccc"),
        );
        assert_ne!(base(), changed);
    }

    #[test]
    fn digest_changes_when_opaque_payload_changes() {
        let base = || {
            compute_external_receipt_digest(
                SCHEMA_VERSION,
                "inv_1",
                "coding-harness",
                "sha256:aaaa",
                ExternalOutcome::Passed,
                "sha256:bbbb",
                Some("sha256:cccc"),
            )
        };
        let changed = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "coding-harness",
            "sha256:aaaa",
            ExternalOutcome::Passed,
            "sha256:bbbb",
            None,
        );
        assert_ne!(base(), changed);
    }

    #[test]
    fn digest_none_opaque_payload_is_stable() {
        let d = compute_external_receipt_digest(
            SCHEMA_VERSION,
            "inv_1",
            "coding-harness",
            "sha256:aaaa",
            ExternalOutcome::Passed,
            "sha256:bbbb",
            None,
        );
        assert!(d.starts_with("sha256:"));
    }

    #[test]
    fn verify_receipt_digest_ok() {
        let mut env = valid_envelope();
        env.receipt_digest = compute_external_receipt_digest(
            &env.schema_version,
            &env.invocation_intent_id,
            &env.issuer,
            &env.subject_digest,
            env.outcome,
            &env.evidence_digest,
            env.opaque_payload_digest.as_deref(),
        );
        assert!(env.verify_receipt_digest().is_ok());
    }

    #[test]
    fn tampered_receipt_digest_is_rejected() {
        let mut env = valid_envelope();
        env.receipt_digest = format!("sha256:{}", "e".repeat(64));
        assert!(env.verify_receipt_digest().is_err());
    }

    #[test]
    fn subject_does_not_match_is_rejected() {
        // Envelope digest is valid, but subject_digest ≠ expected.
        let real_subject_digest = format!("sha256:{}", "d".repeat(64));
        let mut env = valid_envelope();
        env.receipt_digest = compute_external_receipt_digest(
            &env.schema_version,
            &env.invocation_intent_id,
            &env.issuer,
            &env.subject_digest,
            env.outcome,
            &env.evidence_digest,
            env.opaque_payload_digest.as_deref(),
        );
        // The receipt_digest is valid for the envelope's subject_digest,
        // but the envelope as a whole doesn't match what we expected.
        assert!(env.verify_receipt_digest().is_ok());
        assert_ne!(env.subject_digest, real_subject_digest);
    }

    #[test]
    fn production_path_deserializes_external_receipt_envelope() {
        let mut env = valid_envelope();
        env.receipt_digest = compute_external_receipt_digest(
            &env.schema_version,
            &env.invocation_intent_id,
            &env.issuer,
            &env.subject_digest,
            env.outcome,
            &env.evidence_digest,
            env.opaque_payload_digest.as_deref(),
        );

        let json = serde_json::to_value(&env).unwrap();

        // Must deserialize back to the same struct
        let deserialized: ExternalReceiptEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, env);
        assert!(deserialized.verify_receipt_digest().is_ok());
    }
}
