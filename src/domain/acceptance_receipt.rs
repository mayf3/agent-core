//! Generic digest binding for an externally issued acceptance receipt.

use super::external_receipt_envelope::{outcome_str, write_field, ExternalOutcome};
use sha2::{Digest, Sha256};

/// Authenticated Coding Harness principal allowed to issue development
/// acceptance receipts in V1.
pub const TRUSTED_ACCEPTANCE_ISSUER: &str = "harness:coding-harness-v0";

/// Bind an acceptance outcome to the request, candidate, deployable artifact,
/// delivery manifest, and versioned profile contracts. Gate details are
/// intentionally absent: the Kernel validates only this boundary digest.
#[allow(clippy::too_many_arguments)]
pub fn compute_acceptance_binding_digest(
    request_digest: &str,
    candidate_digest: &str,
    artifact_digest: &str,
    manifest_digest: &str,
    outcome: ExternalOutcome,
    contract_catalog_version: &str,
    profile_id: &str,
    profile_catalog_version: &str,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        request_digest,
        candidate_digest,
        artifact_digest,
        manifest_digest,
        outcome_str(outcome),
        contract_catalog_version,
        profile_id,
        profile_catalog_version,
    ] {
        write_field(&mut hasher, value);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}
