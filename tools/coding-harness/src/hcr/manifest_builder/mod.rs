//! Version allocation and delivery manifest construction.
//!
//! Responsibilities moved from Kernel (`coding_task_submit.rs`) to
//! the Coding Harness as part of the external development boundary
//! cleanup (V1 → V2).
//!
//! V1: HookConsumerService → ServiceManifest construction.
//! V2: InvocableCapability → HarnessManifest construction.
//!
//! The `delivery` module dispatches to the correct builder based on
//! the candidate's `target_kind`.  The Kernel receives only opaque
//! content‑addressed bytes — it never parses the manifest type.
//!
//! # Security
//!
//! The version-query credential (`AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN`)
//! must be **read-only** — it shall only permit `GET /v1/components/{id}`
//! (version state), never `POST`, `PUT`, `DELETE`, or any control operation.
//! The Deployment Harness enforces this server-side via token scoping.

pub mod delivery;
pub mod invocable_manifest;
pub mod service_manifest;
pub mod version_allocation;
pub mod version_query;

pub use delivery::build_delivery_manifest;
pub use invocable_manifest::build_invocable_manifest;
pub use service_manifest::build_service_manifest;
pub use version_allocation::{allocate_next_version, increment_patch};
pub use version_query::query_deployed_version;

#[cfg(test)]
pub mod tests;
