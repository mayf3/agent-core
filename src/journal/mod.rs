mod approval;
pub mod capability_activation;
pub mod capability_proposals;
mod conversation;
pub mod grant_ops;
pub mod harness_activation_ops;
mod harness_change_requests;
pub mod harness_ops;
mod hash_chain;
mod outbox;
mod outbox_queue;
mod queue;
mod queue_health;
mod recovery;
mod registry_ops;
mod sqlite;
mod sqlite_read;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
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

pub use sqlite::JournalStore;
