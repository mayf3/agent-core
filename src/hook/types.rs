//! Core Hook ABI v0 types — lifecycle hook kinds, transport bounds,
//! envelopes, and receipts.
//!
//! No product-layer concept (Memory, Dream, Task, Skill, Dashboard)
//! appears in this file. All types are Kernel-generic.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// HookKind — stable lifecycle points
// ---------------------------------------------------------------------------

/// Identifies a well-defined lifecycle point at which the Kernel may invoke
/// an External Harness hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookKind {
    /// Maps a validated platform event to workspace, agent, session, and
    /// policy profiles. Called after Connector validation, before Session
    /// resolution.
    #[serde(rename = "ingress.route.v0")]
    IngressRouteV0,
    /// Unified pre-model call accepting an opaque CandidateInput reference and
    /// returning opaque ordered Context Artifact references.
    #[serde(rename = "context.prepare.v0")]
    ContextPrepareV0,
    /// Resolves an opaque external resource reference.
    #[serde(rename = "context.load.v0")]
    ContextLoadV0,
    /// Observes recorded events or runs so the External Harness can update
    /// its own external state or derived indexes. Prefers pull-based event
    /// cursors; push is a future option.
    #[serde(rename = "event.observe.v0")]
    EventObserveV0,
    /// Evaluates a capability proposal and returns a decision policy result.
    /// Auto-approval still produces a formal Decision event and must not
    /// bypass Gateway digest validation.
    #[serde(rename = "decision.policy.v0")]
    DecisionPolicyV0,
}

// ---------------------------------------------------------------------------
// HookFailureMode
// ---------------------------------------------------------------------------

/// Behaviour when a hook call fails (timeout, unreachable, error response).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookFailureMode {
    /// On failure, allow the operation to proceed without the hook's result
    /// (optimistic degrade).
    #[serde(rename = "fail_open")]
    FailOpen,
    /// On failure, deny or abort the operation (pessimistic safety).
    #[serde(rename = "fail_closed")]
    FailClosed,
    /// On failure, continue with degraded behaviour (e.g. skip optional
    /// enrichment but still serve the request).
    #[serde(rename = "degrade")]
    Degrade,
    /// The hook is not active; it must never be invoked.
    #[serde(rename = "disabled")]
    Disabled,
}

// ---------------------------------------------------------------------------
// HookEndpoint
// ---------------------------------------------------------------------------

/// Transport configuration for a single hook endpoint.
///
/// Currently supports only HTTP(S) URLs. Future variants may include
/// Unix-domain sockets, subprocess commands, or other transports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEndpoint {
    /// The URL of the hook endpoint (e.g. `http://127.0.0.1:9000/hooks/prepare`).
    /// Must be non-empty when the hook is enabled.
    pub url: String,
}

// ---------------------------------------------------------------------------
// HookLimits — per-hook resource bounds
// ---------------------------------------------------------------------------

/// Resource bounds that constrain a single hook invocation.
///
/// Safe defaults ensure the Kernel never hangs or OOMs because of a
/// misconfigured or slow hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookLimits {
    /// Maximum wall-clock time for the hook call, in milliseconds.
    /// Default 5000, max 60_000.
    pub timeout_ms: u64,
    /// Maximum serialised request body size in bytes. Default 1 MiB.
    pub max_request_bytes: u64,
    /// Maximum serialised response body size in bytes. Default 1 MiB.
    pub max_response_bytes: u64,
}

impl Default for HookLimits {
    fn default() -> Self {
        Self {
            timeout_ms: 5_000,
            max_request_bytes: 1024 * 1024,  // 1 MiB
            max_response_bytes: 1024 * 1024, // 1 MiB
        }
    }
}

impl HookLimits {
    /// Returns `Ok(())` if all fields are within hard-coded safety bounds.
    pub fn validate(&self) -> Result<(), HookValidationError> {
        if self.timeout_ms > 60_000 {
            return Err(HookValidationError::LimitExceeded {
                field: "timeout_ms",
                value: self.timeout_ms,
                max: 60_000,
            });
        }
        if self.max_request_bytes > 10 * 1024 * 1024 {
            return Err(HookValidationError::LimitExceeded {
                field: "max_request_bytes",
                value: self.max_request_bytes,
                max: 10 * 1024 * 1024,
            });
        }
        if self.max_response_bytes > 10 * 1024 * 1024 {
            return Err(HookValidationError::LimitExceeded {
                field: "max_response_bytes",
                value: self.max_response_bytes,
                max: 10 * 1024 * 1024,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DecisionPolicyResult
// ---------------------------------------------------------------------------

/// Result returned by a `decision.policy.v0` hook.
///
/// # Security constraints
///
/// - `AutoApprove` is **not** a Gateway bypass. The proposal must still
///   produce a formal Decision event, undergo artifact/manifest digest
///   validation, and pass snapshot activation.
/// - `artifact_digest` / `manifest_digest` checks remain mandatory
///   regardless of the policy result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionPolicyResult {
    /// The proposal requires a human decision; no automatic action taken.
    #[serde(rename = "manual_required")]
    ManualRequired,
    /// The proposal may be auto-approved, subject to full Gateway validation.
    #[serde(rename = "auto_approve")]
    AutoApprove,
    /// The proposal is denied.
    #[serde(rename = "deny")]
    Deny,
    /// Decision is deferred (e.g. await more context, retry later).
    #[serde(rename = "defer")]
    Defer,
}

// ---------------------------------------------------------------------------
// HookRequestEnvelope / HookResponseEnvelope
// ---------------------------------------------------------------------------

/// Generic request envelope sent to a hook endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookRequestEnvelope {
    /// Which hook kind is being invoked.
    pub hook: HookKind,
    /// Unique request identifier for correlation.
    pub request_id: String,
    /// Timestamp when the request was created.
    pub timestamp: DateTime<Utc>,
    /// The hook-specific payload (varies by kind).
    pub payload: Value,
}

/// Generic response envelope returned by a hook endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookResponseEnvelope {
    /// Echoes the request identifier for correlation.
    pub request_id: String,
    /// Which hook kind this response is for.
    pub hook: HookKind,
    /// Timestamp when the response was created.
    pub timestamp: DateTime<Utc>,
    /// The hook-specific result payload (varies by kind).
    pub payload: Value,
}

// ---------------------------------------------------------------------------
// HookCallReceipt — journal evidence for a single hook invocation
// ---------------------------------------------------------------------------

/// Journal evidence recording a single hook invocation attempt.
///
/// Each hook call must produce a receipt that is persisted to the Journal
/// for auditability and debugging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookCallReceipt {
    /// Echoes the request identifier.
    pub request_id: String,
    /// Which hook kind was invoked.
    pub hook: HookKind,
    /// The endpoint URL that was called.
    pub endpoint: String,
    /// When the invocation started.
    pub started_at: DateTime<Utc>,
    /// When the invocation completed (or failed).
    pub completed_at: DateTime<Utc>,
    /// Whether the invocation succeeded from the Kernel's perspective
    /// (i.e. a valid response was received within the configured limits).
    pub success: bool,
    /// Human-readable error message if `success` is false.
    pub error: Option<String>,
    /// Size of the response body in bytes, if received.
    pub response_size_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// HookValidationError
// ---------------------------------------------------------------------------

/// Errors raised when validating hook configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum HookValidationError {
    /// A configured limit exceeds its hard-coded safety bound.
    #[error("hook limit {field} = {value} exceeds maximum allowed {max}")]
    LimitExceeded {
        /// The field name that exceeded the bound.
        field: &'static str,
        /// The configured value.
        value: u64,
        /// The maximum allowed value.
        max: u64,
    },

    /// A required field is empty.
    #[error("hook validation error: {message}")]
    Invalid {
        /// Human-readable description.
        message: String,
    },
}
