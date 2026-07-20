//! Version allocation (patch increment) for managed services.
//!
//! The Coding Harness queries the Deployment Harness for the current
//! successful version and allocates the next patch.  If the component
//! has never been deployed, `allocate_next_version` returns `None`
//! (the generator's default version, e.g. `"0.1.0"`, is used as-is).
//!
//! ⚠ CONCURRENT VERSION ALLOCATION RISK
//!
//! Two concurrent HCR acceptance flows with different
//! `idempotency_key`s may both query the Deployment Harness at the
//! same time, observe the same current version (e.g. `"0.1.0"`), and
//! independently compute the next version (`"0.1.1"`).  Only one
//! succeeds at deployment time; the other fails the Deployment
//! Harness monotonicity check.
//!
//! This is a **known medium-priority debt**:
//!  - No data corruption — the DH rejects the non‑monotonic deployment.
//!  - The next submission after the conflict correctly allocates the
//!    version that follows the first successful deployment.
//!  - The HCR ExecutionStore (file lock) only serialises by the same
//!    `idempotency_key`, not across different keys.
//!
//! The proper fix requires atomic version allocation on the DH side
//! or a distributed lock in the acceptance pipeline.

use anyhow::{anyhow, Result};

/// Increment the patch component of a semver string `"X.Y.Z"`.
///
/// Returns `None` if `current` is not a valid three-part semver or if
/// any component is not a non-negative integer.  Overflow wraps u64.
pub fn increment_patch(current: &str) -> Option<String> {
    let parts: Vec<&str> = current.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let major: u64 = parts[0].parse().ok()?;
    let minor: u64 = parts[1].parse().ok()?;
    let patch: u64 = parts[2].parse().ok()?;
    Some(format!("{major}.{minor}.{}", patch.wrapping_add(1)))
}

/// Allocate the next patch version for a managed-service component.
///
/// - If the component exists on the Deployment Harness: returns
///   `Some(increment_patch(current_version))`.
/// - If the component does not exist: returns `None` (the generator's
///   default version, e.g. `"0.1.0"`, is used as-is).
pub fn allocate_next_version(component_id: &str) -> Result<Option<String>> {
    match super::version_query::query_deployed_version(component_id)? {
        Some(current) => {
            let next = increment_patch(&current)
                .ok_or_else(|| anyhow!("INVALID_EXISTING_VERSION: {current}"))?;
            Ok(Some(next))
        }
        None => Ok(None),
    }
}
