//! SHADOW_SUPPORT_SMOKE_TESTS — Known-failure regression battery.
//!
//! Run: cargo test --test shadow_canary

#![allow(clippy::needless_pass_by_value)]

#[path = "shadow_canary/mod.rs"]
mod shadow_canary;
