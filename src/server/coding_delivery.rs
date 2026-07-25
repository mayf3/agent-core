//! Routing for the direct Agent development loop.
//!
//! Development requests no longer dispatch to external.coding_task_submit.
//! Instead, the message flows through the standard Agent tool loop where
//! the Agent uses workspace tools (read, write, exec) granted via the
//! coding-owner profile to develop code directly.
//!
//! `matches()` detects development requests so the Run carries the
//! generic-development-v1 route tag, but the actual work is done by the
//! Agent in the normal tool loop — not by a synchronous coding pipeline.

use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::LlmClient;
use crate::runtime::{Runtime, RuntimeOutcome};
use anyhow::Result;

/// Detect development requests. The message still flows through the
/// normal Runtime::deliver() Agent loop — no intercept needed anymore.
pub fn matches(_event: &ValidatedEvent) -> bool {
    // All messages now go through the standard Agent tool loop.
    // The Agent has workspace tools (read, write, exec) granted via
    // the coding-owner profile and develops code directly within its
    // own tool scope. No separate coding_deliver path.
    false
}

pub fn deliver<L: LlmClient + 'static>(
    _runtime: &Runtime<L>,
    _journal: &JournalStore,
    _gateway: &Gateway,
    _event: ValidatedEvent,
) -> Result<RuntimeOutcome> {
    unreachable!("coding_delivery::deliver is no longer used; all messages go through Runtime::deliver")
}
