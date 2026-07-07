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
//! ## Current scope (Phase 1 — schema + config only)
//!
//! This module provides only type definitions and configuration parsing.
//! No runtime dispatch, no HTTP calls, no e2e hook execution — those are
//! Phase 2+ concerns.

mod config;
mod types;

#[cfg(test)]
mod tests;

pub use config::*;
pub use types::*;
