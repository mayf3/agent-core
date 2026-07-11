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
