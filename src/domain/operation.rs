//! Operation catalog — the single source of truth for the operations the
//! Kernel knows how to propose, approve, and dispatch.
//!
//! Phase 2 M2a (`docs/decisions/phase2-invocation-gateway-scoping.md`):
//! previously the two operation names (`stdout.send_text`,
//! `feishu.send_message`) were bare string literals duplicated across the
//! gateway allowlist, runtime intent construction, adapter dispatch, and
//! tests. This module consolidates them into one catalog that the gateway,
//! runtime, and adapters reference, so adding or renaming an operation is a
//! single edit. It also carries a `Risk` classification that M2d (durable
//! approval state) will use to decide which operations must pause for human
//! approval.

use serde::{Deserialize, Serialize};

/// The risk classification of an operation. Today every operation is
/// `Write` in effect (it produces a side effect: a reply). M2d will gate
/// `Write` operations behind durable approval state; `ReadOnly` operations
/// (none exist yet — M2e will add the first) will execute inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Risk {
    /// Produces no external side effect (e.g. read a file, get the time).
    /// May execute inline without approval.
    ReadOnly,
    /// Produces a side effect (e.g. send a message). Must go through the
    /// full intent → approval → adapter → receipt chain.
    Write,
}

/// A known operation the Kernel can propose and approve.
#[derive(Debug, Clone, Copy)]
pub struct OperationSpec {
    /// The canonical operation name, used in `InvocationIntent.operation`,
    /// gateway allowlists, and the IPC execute body.
    pub name: &'static str,
    /// Whether the operation is side-effecting. M2d will route `Write`
    /// operations through durable approval state.
    pub risk: Risk,
}

/// The catalog of operations the Kernel currently knows. Add new operations
/// here; reference them via [`lookup`] / [`is_allowed`] instead of bare
/// string literals.
pub const CATALOG: &[OperationSpec] = &[
    OperationSpec {
        name: STDOUT_SEND_TEXT,
        risk: Risk::Write,
    },
    OperationSpec {
        name: FEISHU_SEND_MESSAGE,
        risk: Risk::Write,
    },
];

pub const STDOUT_SEND_TEXT: &str = "stdout.send_text";
pub const FEISHU_SEND_MESSAGE: &str = "feishu.send_message";

/// Look up an operation spec by name. Returns `None` for unknown operations.
pub fn lookup(name: &str) -> Option<&'static OperationSpec> {
    CATALOG.iter().find(|spec| spec.name == name)
}

/// Whether `name` is an operation the gateway is allowed to approve. This is
/// the single source of truth for the gateway allowlist (M2a) — replacing the
/// previous inline `intent.operation != "stdout.send_text" && ...` check.
pub fn is_allowed(name: &str) -> bool {
    lookup(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_both_known_operations() {
        let names: Vec<&str> = CATALOG.iter().map(|spec| spec.name).collect();
        assert!(names.contains(&STDOUT_SEND_TEXT));
        assert!(names.contains(&FEISHU_SEND_MESSAGE));
    }

    #[test]
    fn is_allowed_accepts_catalog_operations_only() {
        assert!(is_allowed(STDOUT_SEND_TEXT));
        assert!(is_allowed(FEISHU_SEND_MESSAGE));
        assert!(!is_allowed("shell.exec"));
        assert!(!is_allowed(""));
    }

    #[test]
    fn known_operations_are_currently_write_risk() {
        // M2a: both existing operations produce side effects. M2e will add
        // the first ReadOnly operation.
        for spec in CATALOG {
            assert_eq!(
                spec.risk,
                Risk::Write,
                "{} should be Write until M2e adds a read-only adapter",
                spec.name
            );
        }
    }
}
