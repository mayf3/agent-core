//! SHADOW_SUPPORT_SMOKE_TESTS — 8 key failure regression tests.
//!
//! Tests marked [INTEGRATION] require full service_decision flow and
//! are verified by the Shadow Canary, not in-memory journal alone.

mod approval; // approval_event_and_intent_are_atomic
mod auth; // missing_owner_open_id_fails_preflight
mod callback; // connector_accepts_deployment_pending, callback_ack_before_deployment_finishes
mod decision; // same_decision_does_not_spawn_second_deployment
mod failure;
mod outbox; // outbox_unknown_idempotent_retry
mod smoke; // support smoke: coding_router, journal queries, hash chain
mod version; // existing_version_allocates_next_patch, equal_version_is_rejected // failure-recovery: activation_failed_does_not_block, rollback_snapshot
