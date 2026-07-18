//! SHA-256 digest helpers shared across the seam.
//!
//! `Sha256Digest` is a tiny validation helper for the canonical
//! `sha256:<64 lowercase hex>` form used everywhere on the seam. The Kernel
//! re-validates any digest it receives rather than trusting the sender.

use serde::{Deserialize, Serialize};

/// Canonical digest prefix.
pub const DIGEST_PREFIX: &str = "sha256:";

/// Hex length of a SHA-256 digest (excluding the prefix).
pub const DIGEST_HEX_LEN: usize = 64;

/// A `sha256:<hex>` digest in canonical form.
///
/// Construct with [`Sha256Digest::from_bytes`] (computes the digest) or
/// [`Sha256Digest::parse`] (validates a wire string). Both produce a
/// canonical lowercase-hex form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sha256Digest(pub String);

impl Sha256Digest {
    /// Compute the digest of `bytes` and return it in canonical form.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let hex = hex::encode(Sha256::digest(bytes));
        Self(format!("{DIGEST_PREFIX}{hex}"))
    }

    /// Validate that `value` is a canonical `sha256:<64 lowercase hex>` string.
    pub fn parse(value: &str) -> Result<Self, DigestError> {
        if !value.starts_with(DIGEST_PREFIX) {
            return Err(DigestError::MissingPrefix);
        }
        let hex_part = &value[DIGEST_PREFIX.len()..];
        if hex_part.len() != DIGEST_HEX_LEN {
            return Err(DigestError::WrongLength);
        }
        if !hex_part
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DigestError::NotLowercaseHex);
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Errors produced by [`Sha256Digest::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestError {
    MissingPrefix,
    WrongLength,
    NotLowercaseHex,
}

impl std::fmt::Display for DigestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DigestError::MissingPrefix => write!(f, "digest must start with '{DIGEST_PREFIX}'"),
            DigestError::WrongLength => write!(
                f,
                "digest hex part must be exactly {DIGEST_HEX_LEN} characters"
            ),
            DigestError::NotLowercaseHex => {
                write!(f, "digest hex part must be lowercase hex (0-9, a-f)")
            }
        }
    }
}

impl std::error::Error for DigestError {}

/// Recompute the canonical `result_digest` of an
/// `ExternalOrchestrationResult`. Every field that participates in the digest
/// is written into the hasher in a fixed order, each followed by a newline,
/// so the digest is deterministic and any field change invalidates it.
///
/// Canonical field order:
///   1. protocol_version
///   2. invocation_id
///   3. outcome
///   4. evidence_ref digest (or empty string when absent)
///   5. result_digest is intentionally EXCLUDED (it is the output).
///
/// `output` is intentionally NOT included: it is free-form controller payload
/// and may be large. Binding the receipt to the controller's *evidence* (via
/// `evidence_ref`) is the integrity anchor, not the verbatim output.
pub fn compute_result_digest(
    protocol_version: &str,
    invocation_id: &str,
    outcome: &str,
    evidence_ref_digest: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(protocol_version.as_bytes());
    hasher.update(b"\n");
    hasher.update(invocation_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(outcome.as_bytes());
    hasher.update(b"\n");
    hasher.update(evidence_ref_digest.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    let hex = hex::encode(hasher.finalize());
    format!("{DIGEST_PREFIX}{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_is_canonical_form() {
        let d = Sha256Digest::from_bytes(b"hello");
        assert!(d.0.starts_with("sha256:"));
        assert_eq!(d.0.len(), DIGEST_PREFIX.len() + DIGEST_HEX_LEN);
        // Known SHA-256 of "hello".
        assert_eq!(
            d.0,
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn parse_accepts_canonical_digest() {
        let s = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(Sha256Digest::parse(s).is_ok());
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        assert_eq!(
            Sha256Digest::parse("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"),
            Err(DigestError::MissingPrefix)
        );
    }

    #[test]
    fn parse_rejects_wrong_length() {
        assert_eq!(
            Sha256Digest::parse("sha256:abc"),
            Err(DigestError::WrongLength)
        );
    }

    #[test]
    fn parse_rejects_uppercase_hex() {
        let s = "sha256:2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824";
        assert_eq!(Sha256Digest::parse(s), Err(DigestError::NotLowercaseHex));
    }

    #[test]
    fn result_digest_is_deterministic() {
        let a = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "succeeded",
            Some("sha256:aaaa"),
        );
        let b = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "succeeded",
            Some("sha256:aaaa"),
        );
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"));
    }

    #[test]
    fn result_digest_changes_when_invocation_changes() {
        let a = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "succeeded",
            Some("sha256:aaaa"),
        );
        let b = compute_result_digest(
            "external-orchestration-v0",
            "inv_2",
            "succeeded",
            Some("sha256:aaaa"),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn result_digest_changes_when_outcome_changes() {
        let a = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "succeeded",
            Some("sha256:aaaa"),
        );
        let b = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "failed",
            Some("sha256:aaaa"),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn result_digest_changes_when_evidence_changes() {
        let a = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "succeeded",
            Some("sha256:aaaa"),
        );
        let b = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "succeeded",
            Some("sha256:bbbb"),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn result_digest_changes_when_evidence_absent() {
        let with_ev = compute_result_digest(
            "external-orchestration-v0",
            "inv_1",
            "succeeded",
            Some("sha256:aaaa"),
        );
        let no_ev = compute_result_digest("external-orchestration-v0", "inv_1", "succeeded", None);
        assert_ne!(with_ev, no_ev);
    }
}
