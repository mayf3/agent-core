//! Hook client trait and fake implementation for testing.
//!
//! `HookClient` abstracts over hook invocation. The production implementation
//! will make real HTTP calls; `FakeHookClient` provides deterministic responses
//! without network access.

use crate::hook::{ContextFragment, HookConfig, HookKind, ResourceRef};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// ContextPrepareRequest / Response
// ---------------------------------------------------------------------------

/// Kernel → External Harness request for context.prepare.v0.
///
/// Carries only Kernel-generic fields — no product-layer concepts (Memory,
/// Dream, Task, Skill, workspace path).
#[derive(Debug, Clone, Serialize)]
pub struct ContextPrepareRequest {
    /// The hook kind being invoked (always `ContextPrepareV0`).
    pub hook: HookKind,
    /// The active Run ID.
    pub run_id: String,
    /// The active Session ID.
    pub session_id: String,
    /// The agent principal ID (e.g. "main").
    pub agent_id: String,
    /// The message sender's principal ID.
    pub principal: String,
    /// The inbound channel (e.g. "cli", "feishu").
    pub channel: String,
    /// The current user message text (truncated for budget).
    pub user_text: String,
    /// Max chars available in the context budget for fragments.
    pub context_budget_chars: usize,
}

/// Response from External Harness after context.prepare.v0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPrepareResponse {
    /// Dynamic context fragments to inject (validated against limits).
    pub fragments: Vec<ContextFragment>,
    /// Opaque resource references for progressive disclosure.
    /// Not loaded in v0 — kept for future context.load.v0.
    pub resource_refs: Vec<ResourceRef>,
}

// ---------------------------------------------------------------------------
// ContextCompressRequest / Response
// ---------------------------------------------------------------------------

/// Kernel → External Provider request for context.compress.v0.
///
/// The Kernel passes the assembled candidate context and model budget;
/// the Provider returns a ContextPlan describing what to keep, truncate,
/// or replace.
#[derive(Debug, Clone, Serialize)]
pub struct ContextCompressRequest {
    /// Always `ContextCompressV0`.
    pub hook: HookKind,
    /// The active Run ID.
    pub run_id: String,
    /// The active Session ID.
    pub session_id: String,
    /// The agent principal ID.
    pub agent_id: String,
    /// Upper bound event sequence for this model invocation.
    pub through_event_id: String,
    /// Model identity string (e.g. "deepseek-v4-flash").
    pub model_identity: String,
    /// Maximum context size for this model in characters.
    pub model_context_budget: usize,
    /// Reserved output budget (model response).
    pub reserved_output_budget: usize,
    /// Candidate context items as a JSON array of context blocks.
    pub candidate_context_items: Vec<Value>,
    /// Opaque scope references for the Provider.
    pub context_scope_refs: Vec<String>,
}

/// A single entry in the ContextPlan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPlanItem {
    /// Index into the original candidate_context_items.
    pub index: usize,
    /// Action: "keep", "truncate", "summarize", "drop"
    pub action: String,
    /// Replacement content (for truncate/summarize).
    pub content: Option<String>,
    /// UTF-8 safe preview bytes (when truncated).
    pub original_bytes: Option<usize>,
    /// SHA-256 digest of the full original content.
    pub digest: Option<String>,
}

/// Provider response for context.compress.v0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompressResponse {
    /// Provider identifier (e.g. "simple-compactor", "test-compactor").
    pub provider_id: String,
    /// Confirmed through_event_id (≤ input).
    pub through_event_id: String,
    /// Mode: "passthrough" or "compacted".
    pub mode: String,
    /// Ordered list of context plan items.
    pub context_items: Vec<ContextPlanItem>,
    /// Estimated total tokens/bytes for this plan.
    pub estimated_size: usize,
    /// Plan digest for audit trail.
    pub plan_digest: String,
    /// Source references for verification.
    pub source_refs: Vec<String>,
}

// ---------------------------------------------------------------------------
// HookClient trait
// ---------------------------------------------------------------------------

/// Abstract interface for invoking external hooks.
///
/// Production implementations send HTTP requests to the configured endpoint.
/// Test implementations use `FakeHookClient`.
pub trait HookClient: std::fmt::Debug {
    /// Call context.prepare.v0 and return fragments + resource refs.
    fn call_context_prepare(
        &self,
        request: &ContextPrepareRequest,
        config: &HookConfig,
    ) -> Result<ContextPrepareResponse>;

    /// Call context.compress.v0 and return a ContextPlan.
    fn call_context_compress(
        &self,
        request: &ContextCompressRequest,
        config: &HookConfig,
    ) -> Result<ContextCompressResponse>;
}

// ---------------------------------------------------------------------------
// FakeHookClient
// ---------------------------------------------------------------------------

/// A hook client that never makes network requests.
///
/// Used in tests and when hooks are disabled. Returns an empty response
/// or configurable fragments for testing.
#[derive(Debug)]
pub struct FakeHookClient {
    /// Fragments to return on the next call (test injection).
    pub fragments: Vec<ContextFragment>,
    /// Resource refs to return on the next call.
    pub resource_refs: Vec<ResourceRef>,
    /// If set, `call_context_prepare` returns this error.
    pub inject_error: Option<String>,
    /// Compress response to return (passthrough by default).
    pub compress_response: Option<ContextCompressResponse>,
    /// If set, `call_context_compress` returns this error.
    pub compress_inject_error: Option<String>,
}

impl FakeHookClient {
    /// Create a client that returns empty responses (hook behaves as disabled).
    pub fn empty() -> Self {
        Self {
            fragments: vec![],
            resource_refs: vec![],
            inject_error: None,
            compress_response: None,
            compress_inject_error: None,
        }
    }

    /// Create a client with pre-configured fragments for testing.
    pub fn with_fragments(fragments: Vec<ContextFragment>) -> Self {
        Self {
            fragments,
            resource_refs: vec![],
            inject_error: None,
            compress_response: None,
            compress_inject_error: None,
        }
    }

    /// Create a client that returns an error.
    pub fn with_error(msg: &str) -> Self {
        Self {
            fragments: vec![],
            resource_refs: vec![],
            inject_error: Some(msg.to_string()),
            compress_response: None,
            compress_inject_error: None,
        }
    }
}

impl HookClient for FakeHookClient {
    fn call_context_prepare(
        &self,
        _request: &ContextPrepareRequest,
        config: &HookConfig,
    ) -> Result<ContextPrepareResponse> {
        // Simulate timeout or error for fail-closed / fail-open tests.
        if let Some(ref msg) = self.inject_error {
            bail!("fake_hook_error:{msg}");
        }

        // Validate fragments against limits.
        let mut valid_fragments = Vec::new();
        for frag in &self.fragments {
            let limits = config.into();
            if let Err(e) = frag.validate_against(&limits) {
                bail!("fake_hook_fragment_validation_failed:{e}");
            }
            if valid_fragments.len() >= config.max_fragments {
                break;
            }
            valid_fragments.push(frag.clone());
        }

        Ok(ContextPrepareResponse {
            fragments: valid_fragments,
            resource_refs: self.resource_refs.clone(),
        })
    }

    fn call_context_compress(
        &self,
        _request: &ContextCompressRequest,
        _config: &HookConfig,
    ) -> Result<ContextCompressResponse> {
        if let Some(ref msg) = self.compress_inject_error {
            bail!("fake_hook_error:context_compress:{msg}");
        }
        if let Some(ref resp) = self.compress_response {
            return Ok(resp.clone());
        }
        // Default: passthrough — return candidate unchanged.
        let items: Vec<ContextPlanItem> = _request
            .candidate_context_items
            .iter()
            .enumerate()
            .map(|(i, _)| ContextPlanItem {
                index: i,
                action: "keep".into(),
                content: None,
                original_bytes: None,
                digest: None,
            })
            .collect();
        let plan_digest = format!("passthrough:{}", _request.through_event_id);
        Ok(ContextCompressResponse {
            provider_id: "fake".into(),
            through_event_id: _request.through_event_id.clone(),
            mode: "passthrough".into(),
            context_items: items,
            estimated_size: 0,
            plan_digest,
            source_refs: vec![],
        })
    }
}
