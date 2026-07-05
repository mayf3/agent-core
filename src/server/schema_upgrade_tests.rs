//! Umbrella module that mounts the schema-upgrade test files as submodules.
//! Keeping the four test files under a single parent module lets `mod.rs`
//! declare one `mod schema_upgrade_tests;` line instead of four, which keeps
//! `server/mod.rs` under the 500-line structure limit. Each submodule reuses
//! the shared `capability_routes_support` helpers via `super::super`.

#[path = "schema_upgrade_coding_ops_tests.rs"]
mod schema_upgrade_coding_ops_tests;
#[path = "schema_upgrade_conflicts_tests.rs"]
mod schema_upgrade_conflicts_tests;
#[path = "schema_upgrade_history_tests.rs"]
mod schema_upgrade_history_tests;
#[path = "schema_upgrade_validation_tests.rs"]
mod schema_upgrade_validation_tests;
