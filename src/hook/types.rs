//! Core Hook ABI v0 types — lifecycle hook kinds, envelope structs,
//! context fragment, resource reference, decision policy result, and
//! invocation receipt.
//!
//! No product-layer concept (Memory, Dream, Task, Skill, Dashboard)
//! appears in this file. All types are Kernel-generic.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// HookKind — the six lifecycle points
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
    /// Builds dynamic context fragments before the Runtime constructs the
    /// model's context window. Receives run, session, principal, and user
    /// input; returns fragments and resource references.
    #[serde(rename = "context.prepare.v0")]
    ContextPrepareV0,
    /// Resolves a `ResourceRef` into full content (progressive disclosure).
    /// The Kernel does not know whether the resource is a skill, memory
    /// item, task, note, or document.
    #[serde(rename = "context.load.v0")]
    ContextLoadV0,
    /// Compresses or summarises context to fit within a token budget.
    /// Part of the context construction path, not the post-run learning path.
    #[serde(rename = "context.compress.v0")]
    ContextCompressV0,
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
    /// Maximum number of `ContextFragment` entries a hook may return.
    /// Default 20, max 100.
    pub max_fragments: usize,
}

impl Default for HookLimits {
    fn default() -> Self {
        Self {
            timeout_ms: 5_000,
            max_request_bytes: 1024 * 1024,  // 1 MiB
            max_response_bytes: 1024 * 1024, // 1 MiB
            max_fragments: 20,
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
        if self.max_fragments > 100 {
            return Err(HookValidationError::LimitExceeded {
                field: "max_fragments",
                value: self.max_fragments as u64,
                max: 100,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ContextFragment — structured dynamic context
// ---------------------------------------------------------------------------

/// The semantic category of a context fragment.
///
/// These are intentionally **not** product-layer concepts (memory, dream,
/// task, skill) — the Kernel does not define product semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextFragmentKind {
    /// An instruction or directive for the model (e.g. "use tool X").
    #[serde(rename = "instruction")]
    Instruction,
    /// A factual statement or data point.
    #[serde(rename = "fact")]
    Fact,
    /// A reference or pointer to external material.
    #[serde(rename = "reference")]
    Reference,
    /// A warning or caution (e.g. "this data may be stale").
    #[serde(rename = "warning")]
    Warning,
    /// A hard constraint the model must obey (e.g. "never reveal the token").
    #[serde(rename = "constraint")]
    Constraint,
}

/// Where the fragment should be placed in the model's context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FragmentPlacement {
    /// Placed below the immutable Kernel system prompt. Only trusted,
    /// allowlisted hooks may produce `SystemAppend` fragments.
    #[serde(rename = "system_append")]
    SystemAppend,
    /// Injected as reference material in the user-context section. Lower
    /// trust level than `SystemAppend`.
    #[serde(rename = "user_context")]
    UserContext,
}

/// Sensitivity level of a context fragment, used for filtering on sensitive
/// channels or during audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FragmentSensitivity {
    /// Suitable for all audiences.
    #[serde(rename = "public")]
    Public,
    /// Safe within the organisation but not for public disclosure.
    #[serde(rename = "internal")]
    Internal,
    /// May contain personal or confidential information.
    #[serde(rename = "sensitive")]
    Sensitive,
    /// Must never be exposed outside a tightly controlled scope (e.g. secrets,
    /// tokens).
    #[serde(rename = "secret")]
    Secret,
}

/// A structured piece of dynamic context injected into the model's context
/// window by a hook.
///
/// # Security constraints
///
/// - **ContextFragment cannot grant permissions.** It is dynamic context,
///   not an authorization mechanism. Tool access, approval bypass, and
///   Gateway decisions are not influenced by fragment content.
/// - **ContextFragment cannot override the immutable system prompt.**
///   The Kernel's base system prompt is always prepended and cannot be
///   shadowed or removed by fragment content.
/// - **ContextFragment cannot bypass Gateway.** Fragments are inputs to
///   the model, not inputs to the Gateway, Capability Host, or Decision
///   system.
/// - **ContextFragment is dynamic context only.** It carries knowledge,
///   instruction, or reference material — never executable policy or
///   permission grants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextFragment {
    /// Unique fragment identifier within the request scope.
    pub id: String,
    /// Which hook produced this fragment (e.g. `"context.prepare.v0"`).
    pub hook_id: String,
    /// Semantic category of the fragment content.
    pub kind: ContextFragmentKind,
    /// Where the fragment should be placed in the context window.
    pub placement: FragmentPlacement,
    /// Priority (higher values = included first within budget).
    pub priority: i32,
    /// The actual text content.
    pub content: String,
    /// Origin description (hook name, file path, etc.).
    pub source: String,
    /// Time-to-live in seconds. `None` means the fragment does not expire.
    pub ttl_secs: Option<u64>,
    /// Estimated token count for context budget management.
    pub estimated_tokens: usize,
    /// Sensitivity level for channel-aware filtering.
    pub sensitivity: FragmentSensitivity,
}

impl ContextFragment {
    /// Validates the fragment against the given resource limits.
    ///
    /// Returns an error if the content size or token estimate exceeds
    /// the allowed bounds.
    pub fn validate_against(&self, limits: &HookLimits) -> Result<(), HookValidationError> {
        let content_bytes = self.content.len() as u64;
        if content_bytes > limits.max_response_bytes {
            return Err(HookValidationError::ContentTooLarge {
                content_size: content_bytes,
                max_bytes: limits.max_response_bytes,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ResourceRef — opaque progressive-disclosure reference
// ---------------------------------------------------------------------------

/// An opaque reference to an external resource that may be loaded on demand
/// via `context.load.v0`.
///
/// The Kernel stores and passes `ResourceRef` values but **does not know**
/// what they represent — they could be skills, memory items, tasks, dreams,
/// documents, or any other product-layer concept owned by the External
/// Harness.
///
/// # Opacity guarantee
///
/// - No field in this struct references Memory, Dream, Task, Skill, or any
///   product-layer concept by name.
/// - The `load_hint` field is an opaque string; the Kernel never interprets
///   its semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceRef {
    /// Unique identifier for this resource. Passed to `context.load.v0` to
    /// fetch the full content.
    pub id: String,
    /// Human-readable title for display in progressive-disclosure UIs.
    pub title: String,
    /// One-line summary of the resource content.
    pub summary: String,
    /// Opaque origin label supplied by the external hook, such as
    /// "ref:guidelines" or "doc:onboarding". The Kernel stores and forwards
    /// this value but does not interpret product-layer semantics.
    pub source: String,
    /// Estimated token cost to load and include this resource. Used for
    /// context budget planning.
    pub estimated_token_cost: usize,
    /// Opaque hint to the External Harness about how to load or prioritise
    /// this resource. The Kernel never inspects its content.
    pub load_hint: Option<String>,
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

/// Errors raised when validating hook configuration or fragment content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum HookValidationError {
    /// Fragment content exceeds the configured `max_response_bytes`.
    #[error("fragment content ({content_size} bytes) exceeds maximum ({max_bytes} bytes)")]
    ContentTooLarge {
        /// Actual content size in bytes.
        content_size: u64,
        /// Allowed maximum in bytes.
        max_bytes: u64,
    },

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
