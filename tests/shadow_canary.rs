//! Shadow Canary — Known-Failure Regression Tests
//!
//! These tests exercise specific failure modes identified during Milestone 1
//! convergence. Each test uses in-memory Journal + CaptureServer + direct
//! function calls — no real HTTP servers or Lima VM required.
//!
//! Run: cargo test --test shadow_canary

#![allow(clippy::needless_pass_by_value)]

#[path = "shadow_canary/mod.rs"]
mod shadow_canary;
