//! Trusted orchestration for catalogued Generic DevelopmentRequests.
//!
//! # Architecture
//!
//! After the external development boundary cleanup (V1), the Kernel
//! no longer allocates component versions or constructs delivery
//! manifests for managed services.  These responsibilities moved to
//! the Coding Harness acceptance pipeline.
//!
//! The Kernel only:
//! - Validates source binding, identity, and idempotency
//! - Verifies manifest digest consistency (no manifest type parsing)
//! - Records proposals with receipt-bound refs/digests
//! - Persists governance facts (approval, deployment intent, registry)

pub mod handler;
pub mod invocation_journal;

#[cfg(test)]
pub mod tests;

pub use handler::{handle_coding_task_submit, CodingHarnessRejection, CodingTaskSubmitResult};
