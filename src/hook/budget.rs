//! Generic `run.budget.resolve.v0` hook contract.
//!
//! The Kernel calls this hook once at Run creation to obtain a frozen budget
//! decision (max tool rounds, max wall-clock time, exhaustion action). The
//! decision is validated against a host-level safety ceiling, frozen onto the
//! Run, and enforced by the tool recall loop.
//!
//! No product-layer concept (Coding, Ops, Router, task complexity, checkpoint)
//! appears here. The hook only returns raw governance numbers.

use anyhow::{bail, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Host safety ceiling — the ONLY Kernel-side hardcoded budget bound.
// ---------------------------------------------------------------------------

/// Host-level safety ceiling that no budget hook (default or external) may
/// exceed. This is **not** a product policy — it prevents a misconfigured or
/// hostile hook from returning infinite/unbounded values that would hang the
/// Kernel. Derived from the pre-existing config validation bounds so existing
/// deployments see no behaviour change.
///
/// These constants are the absolute maximum the Kernel will accept from any
/// hook. A normal product policy (set by the default hook or an external hook)
/// will typically be well below these values.
pub const HOST_MAX_TOOL_ROUNDS: u32 = 64;
pub const HOST_MAX_WALL_TIME_MS: u64 = 600_000;

/// Validates a [`RunBudgetDecision`] against the host safety ceiling.
pub fn validate_against_ceiling(decision: &RunBudgetDecision) -> Result<()> {
    if decision.max_tool_rounds == 0 {
        bail!("budget_max_tool_rounds_zero");
    }
    if decision.max_tool_rounds > HOST_MAX_TOOL_ROUNDS {
        bail!(
            "budget_max_tool_rounds_exceeds_ceiling: {} > {}",
            decision.max_tool_rounds,
            HOST_MAX_TOOL_ROUNDS
        );
    }
    if decision.max_wall_time_ms == 0 {
        bail!("budget_max_wall_time_ms_zero");
    }
    if decision.max_wall_time_ms > HOST_MAX_WALL_TIME_MS {
        bail!(
            "budget_max_wall_time_ms_exceeds_ceiling: {} > {}",
            decision.max_wall_time_ms,
            HOST_MAX_WALL_TIME_MS
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ExhaustionAction
// ---------------------------------------------------------------------------

/// What the Kernel does when the budget is exhausted (max rounds reached or
/// wall-clock timeout exceeded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExhaustionAction {
    /// End the Run with an explicit budget-exhausted terminal state. The Run
    /// is marked Failed. The user sees a clear "budget exhausted" message and
    /// is NOT told to send "继续".
    #[serde(rename = "terminate")]
    Terminate,
    /// End the current Run normally so the partial reply is delivered. The
    /// user is told they may send "继续" to start a new Run and continue. This
    /// reproduces the pre-V0 yield behaviour.
    #[serde(rename = "yield")]
    Yield,
}

// ---------------------------------------------------------------------------
// RunBudgetDecision — the frozen hook decision
// ---------------------------------------------------------------------------

/// The budget decision returned by a `run.budget.resolve.v0` hook (or the
/// default hook). Frozen onto the Run at creation and never changed mid-Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBudgetDecision {
    /// Maximum number of tool-call rounds for this Run.
    pub max_tool_rounds: u32,
    /// Maximum wall-clock time for the entire tool recall loop, in ms.
    pub max_wall_time_ms: u64,
    /// What to do when the budget is exhausted.
    pub exhaustion_action: ExhaustionAction,
}

impl RunBudgetDecision {
    /// Canonical SHA-256 digest over the decision fields. Used for audit and
    /// to detect mid-Run tampering.
    pub fn digest(&self) -> String {
        let action_str = match self.exhaustion_action {
            ExhaustionAction::Terminate => "terminate",
            ExhaustionAction::Yield => "yield",
        };
        let mut hasher = Sha256::new();
        hasher.update((self.max_tool_rounds as u64).to_be_bytes().as_slice());
        hasher.update(self.max_wall_time_ms.to_be_bytes().as_slice());
        hasher.update(action_str.as_bytes());
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

// ---------------------------------------------------------------------------
// Request / response envelopes
// ---------------------------------------------------------------------------

/// Request sent to a `run.budget.resolve.v0` hook. Contains only generic
/// governance context — no natural-language requirement, product type, or
/// task complexity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBudgetHookRequest {
    pub request_id: String,
    pub principal: String,
    pub session_id: String,
    pub run_id: String,
    pub registry_snapshot_id: String,
    /// SHA-256 digest over the sorted operation names in the pinned snapshot.
    /// Lets the hook make decisions based on *what capabilities* are available
    /// without the Kernel revealing product semantics.
    pub operations_digest: String,
}

/// Hook response envelope. The Kernel validates the decision against the host
/// ceiling before accepting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBudgetHookResponse {
    pub request_id: String,
    pub run_id: String,
    pub decision: RunBudgetDecision,
}

impl RunBudgetHookResponse {
    pub fn validate_against(&self, request: &RunBudgetHookRequest) -> Result<()> {
        if self.request_id != request.request_id {
            bail!("budget_response_request_id_mismatch");
        }
        if self.run_id != request.run_id {
            bail!("budget_response_run_id_mismatch");
        }
        validate_against_ceiling(&self.decision)?;
        Ok(())
    }

    /// HMAC authentication message. The provider signs this with the shared
    /// secret; the Kernel verifies it.
    pub fn authentication_message(&self, provider_id: &str) -> Vec<u8> {
        let action_str = match self.decision.exhaustion_action {
            ExhaustionAction::Terminate => "terminate",
            ExhaustionAction::Yield => "yield",
        };
        let mut bytes = Vec::new();
        append_field(&mut bytes, provider_id.as_bytes());
        append_field(&mut bytes, self.request_id.as_bytes());
        append_field(&mut bytes, self.run_id.as_bytes());
        append_field(
            &mut bytes,
            self.decision.max_tool_rounds.to_string().as_bytes(),
        );
        append_field(
            &mut bytes,
            self.decision.max_wall_time_ms.to_string().as_bytes(),
        );
        append_field(&mut bytes, action_str.as_bytes());
        bytes
    }
}

/// Authenticated response after HMAC proof verification. The `provider_id`
/// comes from the trusted local binding — never from response JSON.
#[derive(Debug, Clone)]
pub struct AuthenticatedRunBudgetResponse {
    pub provider_id: String,
    pub request_id: String,
    pub response: RunBudgetHookResponse,
}

/// Compute the operations digest from a list of operation names.
pub fn compute_operations_digest(operation_names: &[String]) -> String {
    let mut sorted: Vec<&String> = operation_names.iter().collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    for name in &sorted {
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

// ---------------------------------------------------------------------------
// HMAC proof helpers (mirrors context_artifact.rs pattern)
// ---------------------------------------------------------------------------

pub fn compute_budget_provider_proof(secret: &str, message: &[u8]) -> Result<String> {
    if secret.is_empty() {
        bail!("budget_provider_credential_empty");
    }
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| anyhow::anyhow!("hmac_key"))?;
    mac.update(message);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

pub fn verify_budget_provider_proof(secret: &str, message: &[u8], proof: &str) -> Result<()> {
    let proof = hex::decode(proof).map_err(|_| anyhow::anyhow!("provider_proof_encoding"))?;
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| anyhow::anyhow!("hmac_key"))?;
    mac.update(message);
    mac.verify_slice(&proof)
        .map_err(|_| anyhow::anyhow!("provider_authentication_failed"))
}

fn append_field(target: &mut Vec<u8>, field: &[u8]) {
    target.extend_from_slice(&(field.len() as u64).to_be_bytes());
    target.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_decision() -> RunBudgetDecision {
        RunBudgetDecision {
            max_tool_rounds: 12,
            max_wall_time_ms: 300_000,
            exhaustion_action: ExhaustionAction::Yield,
        }
    }

    #[test]
    fn decision_digest_is_deterministic() {
        let d = sample_decision();
        let d2 = sample_decision();
        assert_eq!(d.digest(), d2.digest());
        assert!(d.digest().starts_with("sha256:"));
    }

    #[test]
    fn decision_digest_changes_with_fields() {
        let base = sample_decision();
        let different_rounds = RunBudgetDecision {
            max_tool_rounds: 64,
            ..base.clone()
        };
        let different_time = RunBudgetDecision {
            max_wall_time_ms: 600_000,
            ..base.clone()
        };
        let different_action = RunBudgetDecision {
            exhaustion_action: ExhaustionAction::Terminate,
            ..base.clone()
        };
        assert_ne!(base.digest(), different_rounds.digest());
        assert_ne!(base.digest(), different_time.digest());
        assert_ne!(base.digest(), different_action.digest());
    }

    #[test]
    fn validate_accepts_within_ceiling() {
        assert!(validate_against_ceiling(&sample_decision()).is_ok());
        let max = RunBudgetDecision {
            max_tool_rounds: HOST_MAX_TOOL_ROUNDS,
            max_wall_time_ms: HOST_MAX_WALL_TIME_MS,
            exhaustion_action: ExhaustionAction::Terminate,
        };
        assert!(validate_against_ceiling(&max).is_ok());
    }

    #[test]
    fn validate_rejects_zero_rounds() {
        let d = RunBudgetDecision {
            max_tool_rounds: 0,
            ..sample_decision()
        };
        assert!(validate_against_ceiling(&d).is_err());
    }

    #[test]
    fn validate_rejects_zero_wall_time() {
        let d = RunBudgetDecision {
            max_wall_time_ms: 0,
            ..sample_decision()
        };
        assert!(validate_against_ceiling(&d).is_err());
    }

    #[test]
    fn validate_rejects_over_ceiling_rounds() {
        let d = RunBudgetDecision {
            max_tool_rounds: HOST_MAX_TOOL_ROUNDS + 1,
            ..sample_decision()
        };
        assert!(validate_against_ceiling(&d).is_err());
    }

    #[test]
    fn validate_rejects_over_ceiling_wall_time() {
        let d = RunBudgetDecision {
            max_wall_time_ms: HOST_MAX_WALL_TIME_MS + 1,
            ..sample_decision()
        };
        assert!(validate_against_ceiling(&d).is_err());
    }

    #[test]
    fn operations_digest_is_order_independent() {
        let ops_a = vec!["b".to_string(), "a".to_string(), "c".to_string()];
        let ops_b = vec!["c".to_string(), "a".to_string(), "b".to_string()];
        assert_eq!(
            compute_operations_digest(&ops_a),
            compute_operations_digest(&ops_b)
        );
    }

    #[test]
    fn provider_proof_roundtrip() {
        let response = RunBudgetHookResponse {
            request_id: "req".into(),
            run_id: "run".into(),
            decision: sample_decision(),
        };
        let proof =
            compute_budget_provider_proof("secret", &response.authentication_message("provider"))
                .unwrap();
        assert!(verify_budget_provider_proof(
            "secret",
            &response.authentication_message("provider"),
            &proof
        )
        .is_ok());
        // Wrong secret fails
        assert!(verify_budget_provider_proof(
            "wrong",
            &response.authentication_message("provider"),
            &proof
        )
        .is_err());
    }

    #[test]
    fn response_validate_rejects_mismatched_ids() {
        let request = RunBudgetHookRequest {
            request_id: "req1".into(),
            principal: "p".into(),
            session_id: "s".into(),
            run_id: "run1".into(),
            registry_snapshot_id: "snap".into(),
            operations_digest: "sha256:abc".into(),
        };
        let response = RunBudgetHookResponse {
            request_id: "req2".into(), // mismatch
            run_id: "run1".into(),
            decision: sample_decision(),
        };
        assert!(response.validate_against(&request).is_err());
    }
}
