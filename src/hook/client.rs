//! Hook client trait and fake implementation for testing.
//!
//! `HookClient` abstracts over hook invocation. The production implementation
//! will make real HTTP calls; `FakeHookClient` provides deterministic responses
//! without network access.

use crate::hook::{ContextFragment, HookConfig, HookKind, ResourceRef};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

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
}

impl FakeHookClient {
    /// Create a client that returns empty responses (hook behaves as disabled).
    pub fn empty() -> Self {
        Self {
            fragments: vec![],
            resource_refs: vec![],
            inject_error: None,
        }
    }

    /// Create a client with pre-configured fragments for testing.
    pub fn with_fragments(fragments: Vec<ContextFragment>) -> Self {
        Self {
            fragments,
            resource_refs: vec![],
            inject_error: None,
        }
    }

    /// Create a client that returns an error.
    pub fn with_error(msg: &str) -> Self {
        Self {
            fragments: vec![],
            resource_refs: vec![],
            inject_error: Some(msg.to_string()),
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
}
