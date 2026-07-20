mod activation_core;
mod approval;
pub mod capability_activation;
mod capability_approvals;
mod capability_proposal_hcr;
pub mod capability_proposals;
mod coding_task_submissions;
pub(crate) mod component_control;
mod component_registry;
mod conversation;
pub mod event_observe;
pub mod grant_ops;
pub mod harness_activation_ops;
mod harness_change_requests;
pub mod harness_ops;
pub(crate) mod hash_chain;
mod hcr_settlement;
mod model_invocation;
mod outbox;
mod outbox_queue;
mod queue;
mod queue_health;
mod recovery;
mod registry_control_upgrade;
mod registry_ops;
mod sqlite;
mod sqlite_read;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
pub mod trusted_capability_activation;
mod trusted_capability_validation;
pub(crate) mod trusted_service_activation;
mod unknown;
mod worker;

#[cfg(test)]
#[path = "tests/registry_retirement.rs"]
mod registry_retirement;

#[cfg(test)]
#[path = "tests/manifest_idempotent.rs"]
mod manifest_idempotent;

#[cfg(test)]
#[path = "tests/capability_concurrency.rs"]
mod capability_concurrency;

#[cfg(test)]
#[path = "tests/grant_ops.rs"]
mod grant_ops_tests;

#[cfg(test)]
#[path = "tests/grant_ops_lifecycle.rs"]
mod grant_ops_lifecycle_tests;

#[cfg(test)]
#[path = "tests/hcr_claim.rs"]
mod hcr_claim_tests;

pub(crate) use coding_task_submissions::CodingTaskSubmissionClaim;
pub use event_observe::{
    redact_payload, EventObserveQuery, EventObserveResponse, ObservedEvent, DEFAULT_OBSERVE_LIMIT,
    MAX_OBSERVE_LIMIT, OBSERVE_SCHEMA_VERSION,
};
pub use sqlite::JournalStore;
