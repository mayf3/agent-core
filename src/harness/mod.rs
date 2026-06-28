//! PR 2A: External Harness Registry Control Plane.
//!
//! This module manages immutable bundle manifests, mutable runtime
//! registrations, explicit channel operation grants, candidate snapshot
//! composition, and activation/rollback. No network calls to harness
//! endpoints are made in PR 2A.

pub mod admin;
pub mod grants;
pub mod manifest;
pub mod registration;
