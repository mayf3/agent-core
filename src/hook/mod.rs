//! Hook ABI v0 — Kernel–External Harness extension boundary.
//!
//! Defines the generic hook types that the Kernel exposes so an External
//! Harness can register callbacks at well-defined lifecycle points without
//! the Kernel knowing product-layer semantics (Memory, Dream, Task, Skill,
//! Dashboard, …).
//!
//! ## Design constraints
//!
//! - Kernel defines the **types**, not the product-layer behaviour.
//! - Hook configuration is owned by the Kernel; hook implementations are
//!   owned by the External Harness.
//! - Hooks are invoked at fixed lifecycle points; the Kernel never blindly
//!   forwards every event to every hook.
//!
//! ## Current scope (Phase 1 — schema + config, Phase 2 — client + Runtime)
//!
//! Phase 1 (schema + config): types and configuration parsing, no dispatch.
//! Phase 2 (this PR): HookClient trait + FakeHookClient + context.prepare
//! integration in the Runtime, still no real HTTP.

mod client;
mod config;
mod http_client;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod context_tests;

#[cfg(test)]
mod http_tests;

pub use client::*;
pub use config::*;
pub use http_client::*;
pub use types::*;
