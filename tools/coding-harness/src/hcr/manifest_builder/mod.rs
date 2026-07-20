//! Version allocation and delivery manifest construction.
//!
//! Responsibilities moved from Kernel (`coding_task_submit.rs`) to
//! the Coding Harness as part of the external development boundary
//! cleanup (V1).  The Harness queries the Deployment Harness (read-only
//! version endpoint), allocates the next patch version, and constructs
//! the final delivery `ServiceManifest`.
//!
//! # Security
//!
//! The version-query credential (`AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN`)
//! must be **read-only** — it shall only permit `GET /v1/components/{id}`
//! (version state), never `POST`, `PUT`, `DELETE`, or any control operation.
//! The Deployment Harness enforces this server-side via token scoping.

pub mod delivery_manifest;
pub mod version_allocation;
pub mod version_query;

pub use delivery_manifest::build_delivery_manifest;
pub use version_allocation::{allocate_next_version, increment_patch};
pub use version_query::query_deployed_version;

#[cfg(test)]
pub mod tests;
