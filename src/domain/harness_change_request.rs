//! Domain types for HarnessChangeRequest and HCR claims.
//!
//! A durable record stored in the `harness_change_requests` table.
//! Created by PR4A1 without a Run; R2 adds atomic claim + Run binding.

use serde::{Deserialize, Serialize};

/// A durable HarnessChangeRequest record, stored in the
/// `harness_change_requests` table. Created by PR4A1 without a Run;
/// R2 consumes pending requests and creates Runs via atomic claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessChangeRequest {
    pub request_id: String,
    pub source: String,
    pub source_message_id: String,
    pub session_id: String,
    pub principal_id: String,
    pub channel: String,
    pub chat_type: String,
    pub harness_id: String,
    pub requirement: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub run_id: Option<String>,
    pub error_code: Option<String>,
}

macro_rules! claim_id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, uuid::Uuid::new_v4().simple()))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

claim_id_type!(ClaimId, "claim");

/// The status of an HCR claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HcrClaimStatus {
    Active,
    Released,
}

impl HcrClaimStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            HcrClaimStatus::Active => "active",
            HcrClaimStatus::Released => "released",
        }
    }

    pub fn parse_opt(s: &str) -> Option<Self> {
        match s {
            "active" => Some(HcrClaimStatus::Active),
            "released" => Some(HcrClaimStatus::Released),
            _ => None,
        }
    }
}

/// A claim record for an HCR. Created atomically when a worker claims an HCR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcrClaim {
    pub claim_id: ClaimId,
    pub hcr_id: String,
    pub harness_id: String,
    pub worker_instance_id: String,
    pub claimed_at: String,
    pub status: HcrClaimStatus,
}

/// A binding between an HCR claim and a Run. Created after a successful claim
/// to bind the worker's Run to the claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcrRunBinding {
    pub binding_id: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub created_at: String,
}

/// The kind of a trusted acceptance gate for HCR settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Scaffold,
    Build,
    TrustedTest,
    TrustedSmoke,
    Artifact,
}

impl GateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GateKind::Scaffold => "scaffold",
            GateKind::Build => "build",
            GateKind::TrustedTest => "trusted_test",
            GateKind::TrustedSmoke => "trusted_smoke",
            GateKind::Artifact => "artifact",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "scaffold" => Some(GateKind::Scaffold),
            "build" => Some(GateKind::Build),
            "trusted_test" => Some(GateKind::TrustedTest),
            "trusted_smoke" => Some(GateKind::TrustedSmoke),
            "artifact" => Some(GateKind::Artifact),
            _ => None,
        }
    }

    /// All required gates in execution order.
    pub fn all_required() -> &'static [GateKind] {
        &[
            GateKind::Scaffold,
            GateKind::Build,
            GateKind::TrustedTest,
            GateKind::TrustedSmoke,
            GateKind::Artifact,
        ]
    }
}

/// Canonical service-side gate attempt. Created by `prepare_hcr_gate_attempt`.
/// Records what operation/profile/workspace/harness this gate is expected to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcrGateAttempt {
    pub gate_attempt_id: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub harness_id: String,
    pub workspace_id: String,
    pub gate_kind: String,
    pub expected_operation: String,
    pub expected_profile: String,
    pub invocation_intent_id: String,
    pub created_at: String,
}

/// Durable record of a single gate execution, bound to its receipt.
/// Only created by the trusted `register_gate_evidence` entry point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcrGateEvidence {
    pub evidence_id: String,
    pub gate_attempt_id: String,
    pub receipt_event_id: String,
    pub structured_status: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub child_cleanup: Option<bool>,
    pub error_code: Option<String>,
    pub receipt_payload_digest: String,
    pub created_at: String,
}

/// A terminal settlement record for an HCR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcrSettlement {
    pub settlement_id: String,
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub result: String,
    pub error_code: Option<String>,
    pub evidence_set_digest: String,
    pub created_at: String,
}

/// Result of a settlement attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementResult {
    /// All gates passed. Carries settlement_id.
    Succeeded(String),
    /// Candidate code failed. Carries settlement_id.
    CandidateFailed(String),
    /// Infrastructure failure; HCR remains running. Carries error message.
    InfrastructureFailure(String),
    /// HCR was already settled. Carries existing result description.
    AlreadySettled(String),
    /// Evidence set is incomplete or invalid. Carries error message.
    EvidenceIncomplete(String),
    /// Conflicting evidence detected. Carries conflict message.
    EvidenceConflict(String),
}
