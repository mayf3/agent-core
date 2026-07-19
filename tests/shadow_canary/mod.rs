//! Shadow Canary — Known-Failure Regression Tests
//!
//! Test modules:
//! - preflight:   Owner validation, outbox retry
//! - versioning:  Version monotonicity, allocation, idempotency
//! - readiness:   Event page readiness, token validation, kernel availability
//! - approval:    Approval atomicity, connector deployment_pending acceptance
//! - deployment:  deployment_pending response, callback ACK, in-flight tracking
//! - recovery:    ActivationFailed isolation, origin validation, schema

mod helpers;

mod preflight;
mod versioning;
mod readiness;
mod approval;
mod deployment;
mod recovery;
