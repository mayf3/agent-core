//! Protocol version stamp. Bumped only on a breaking change to the DTOs in
//! this crate or to the canonical digest scheme. The Kernel and any external
//! Controller MUST agree on this value before exchanging an intent/result.

use serde::{Deserialize, Serialize};

/// Human-readable protocol identifier for Seam V0.
pub const PROTOCOL_VERSION: &str = "external-orchestration-v0";

/// Carries the protocol version on the wire so both sides can reject a
/// mismatch up front rather than misinterpreting fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion(pub String);

impl ProtocolVersion {
    /// The current supported protocol version.
    pub fn current() -> Self {
        Self(PROTOCOL_VERSION.to_string())
    }

    /// True when the wire value matches the currently supported version.
    pub fn is_current(&self) -> bool {
        self.0 == PROTOCOL_VERSION
    }
}

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self::current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_is_stable_string() {
        assert_eq!(ProtocolVersion::current().0, PROTOCOL_VERSION);
        assert!(ProtocolVersion::current().is_current());
    }

    #[test]
    fn mismatched_version_is_not_current() {
        let other = ProtocolVersion("external-orchestration-v999".into());
        assert!(!other.is_current());
    }

    #[test]
    fn protocol_version_round_trips_serde() {
        let v = ProtocolVersion::current();
        let json = serde_json::to_string(&v).expect("serialize");
        let back: ProtocolVersion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back);
    }
}
