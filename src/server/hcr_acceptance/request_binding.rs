//! HCR requirement binding — extracts development_request and computes
//! requirement_digest for the acceptance invocation.
//!
//! The Kernel loads the HCR requirement (which was persisted by handler.rs
//! as opaque JSON), computes its SHA-256 digest, and passes both the
//! requirement digest and the embedded development_request to the Coding
//! Harness via the InvocationIntent arguments.  The Kernel does NOT parse,
//! validate, or understand any product-specific fields in the requirement.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Builder result containing the parsed development_request value and
/// the computed requirement digest.
pub struct RequirementBinding {
    /// The development_request extracted from the HCR requirement JSON
    /// as an opaque Value (Kernel does not deserialize it).
    pub development_request: Option<Value>,
    /// SHA-256 digest of the full HCR requirement string (sha256:<hex>).
    pub requirement_digest: String,
}

/// Parse an HCR requirement string and extract the binding fields needed
/// for the acceptance invocation.
///
/// The requirement is treated as opaque JSON bytes — the Kernel only
/// navigates to the `development_request` key and computes a digest over
/// the full raw string.  No product-specific field validation occurs here.
pub fn build_requirement_binding(requirement: &str) -> RequirementBinding {
    let requirement_digest = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(requirement.as_bytes()))
    );

    let development_request: Option<Value> = serde_json::from_str(requirement)
        .ok()
        .and_then(|v: Value| v.get("development_request").cloned());

    RequirementBinding {
        development_request,
        requirement_digest,
    }
}
