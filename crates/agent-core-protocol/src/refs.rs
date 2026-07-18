//! Generic, opaque identity / reference types.
//!
//! These are intentionally string-newtypes: the Kernel and Controller exchange
//! them as opaque tokens. Neither side parses structure out of them beyond
//! equality and emptiness checks.

use serde::{Deserialize, Serialize};

/// Opaque identifier for a single Kernel invocation. Bound to an
/// `InvocationIntent` and echoed back on the matching
/// `ExternalOrchestrationResult`; the Kernel rejects any result whose
/// `invocation_id` does not match the intent it issued.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InvocationId(pub String);

impl InvocationId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Opaque identifier for a Kernel Run. Carried for observability/correlation
/// only; seam V0 does not key receipt storage on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

impl RunId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque reference to the authenticated principal that initiated the Run.
/// Seam V0 carries this for audit only — the Kernel has already authenticated
/// the principal before it issues an intent; the Controller trusts the
/// Kernel-bound context and never re-authenticates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrincipalRef(pub String);

impl PrincipalRef {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque, digest-bearing reference to externally-stored content
/// (context blobs, evidence bundles, etc.). The Kernel stores the digest but
/// does NOT parse the payload — the Controller owns the interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpaqueRef {
    /// Free-form kind label set by the Controller (e.g. "context", "evidence").
    /// The Kernel treats it as an audit label only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Content digest in canonical `sha256:<hex>` form.
    pub digest: String,
}

impl OpaqueRef {
    pub fn new(kind: impl Into<String>, digest: impl Into<String>) -> Self {
        Self {
            kind: Some(kind.into()),
            digest: digest.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_id_round_trips() {
        let id = InvocationId::new("inv_123");
        let json = serde_json::to_string(&id).expect("serialize");
        assert!(json.contains("inv_123"));
        let back: InvocationId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    #[test]
    fn empty_invocation_id_detected() {
        assert!(InvocationId::new("").is_empty());
        assert!(!InvocationId::new("x").is_empty());
    }

    #[test]
    fn opaque_ref_serializes_kind_and_digest() {
        let r = OpaqueRef::new("context", "sha256:abc");
        let v = serde_json::to_value(&r).expect("serialize");
        assert_eq!(v["kind"], serde_json::json!("context"));
        assert_eq!(v["digest"], serde_json::json!("sha256:abc"));
    }

    #[test]
    fn opaque_ref_kind_is_optional_on_wire() {
        // Deserialization must tolerate a missing `kind`.
        let v = serde_json::json!({ "digest": "sha256:abc" });
        let r: OpaqueRef = serde_json::from_value(v).expect("deserialize");
        assert_eq!(r.kind, None);
        assert_eq!(r.digest, "sha256:abc");
    }
}
