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

use crate::domain::{CapabilityGrant, ChannelKind};

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
    OperationSpec {
        name: TIME_NOW,
        risk: Risk::ReadOnly,
    },
];

pub const STDOUT_SEND_TEXT: &str = "stdout.send_text";
pub const FEISHU_SEND_MESSAGE: &str = "feishu.send_message";
/// Read-only: return the current kernel wall-clock time. The first
/// `Risk::ReadOnly` operation (Phase 2 M2e) — it produces no side effect and
/// so may execute inline without approval. Implemented by `TimeAdapter`.
pub const TIME_NOW: &str = "time.now";

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

/// The capability grants a run principal receives for a given channel.
///
/// Phase 2 M2b (`docs/decisions/phase2-invocation-gateway-scoping.md`):
/// previously each gateway ingress branch (`validate_cli_ingress`,
/// `validate_feishu_ingress`, `recover_feishu_event`, and the `cli_principal`
/// helper) hardcoded a single `CapabilityGrant` inline, duplicating the
/// channel → operation mapping in four places. This type centralizes that
/// mapping so the grant set a principal receives is derived from one place.
///
/// [`ExecutionProfile::for_channel`] returns the baseline grant each branch
/// hardcoded (behavior-preserving). [`ExecutionProfile::with_extra`] then
/// augments it with operator-configured extra operations, closing M2b's
/// config-driven exit criterion (cli/feishu grant set configurable via
/// `KernelConfig`). With no extra operations configured the profile is
/// identical to the previous inline literals.
#[derive(Debug, Clone)]
pub struct ExecutionProfile {
    pub grants: Vec<CapabilityGrant>,
}

impl ExecutionProfile {
    /// Derive the baseline capability grants for `channel`. Each grant is
    /// scoped to `current_session` (the run may only act within its own
    /// session). This is the single source of truth referenced by every
    /// gateway ingress branch.
    pub fn for_channel(channel: ChannelKind) -> Self {
        let operation = match channel {
            ChannelKind::Cli => STDOUT_SEND_TEXT,
            ChannelKind::Feishu => FEISHU_SEND_MESSAGE,
        };
        Self {
            grants: vec![CapabilityGrant {
                operation: operation.to_string(),
                scope: "current_session".to_string(),
            }],
        }
    }

    /// Augment the baseline profile with extra catalog-allowed operations
    /// supplied by the operator. Phase 2 M2b's config-driven half: an operator
    /// may widen a channel's grants via `KernelConfig` without editing code.
    ///
    /// Each entry in `extra_operations` must be a name in [`CATALOG`]; unknown
    /// names are silently dropped (they cannot be approved anyway, because the
    /// gateway allowlist is the catalog — dropping here keeps the grant set
    /// honest). Extra grants are scoped to `current_session`, matching the
    /// baseline scope invariant.
    ///
    /// This is additive: an unknown/empty `extra_operations` leaves the profile
    /// identical to [`ExecutionProfile::for_channel`], so the default is
    /// behavior-preserving.
    pub fn with_extra(mut self, extra_operations: &[String]) -> Self {
        for name in extra_operations {
            if name.is_empty() {
                continue;
            }
            if lookup(name).is_none() {
                continue;
            }
            let already = self.grants.iter().any(|g| &g.operation == name);
            if !already {
                self.grants.push(CapabilityGrant {
                    operation: name.clone(),
                    scope: "current_session".to_string(),
                });
            }
        }
        self
    }
}

/// Render the operation catalog as a compact tool-catalog text block for the
/// LLM context (Phase 2 tool-surfacing foundation). Each line is
/// `<name> (risk: <ReadOnly|Write>) — <one-line intent>`. Only catalogued
/// operations appear; the model is told these are the only operations it may
/// propose. Today this surfaces `time.now` (ReadOnly) alongside the two reply
/// operations (Write). Surfacing is additive — proposing/executing an
/// operation still goes through the existing intent → policy → adapter chain.
pub fn catalog_for_context() -> String {
    CATALOG
        .iter()
        .map(|spec| {
            let risk = match spec.risk {
                Risk::ReadOnly => "ReadOnly",
                Risk::Write => "Write",
            };
            let desc = match spec.name {
                STDOUT_SEND_TEXT => "send a text reply to the user (stdout).",
                FEISHU_SEND_MESSAGE => "send a message reply to the Feishu chat.",
                TIME_NOW => "read the current kernel wall-clock time (no side effect).",
                _ => "catalogued operation.",
            };
            format!("{} (risk: {}) — {}", spec.name, risk, desc)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_for_context_lists_every_catalogued_operation_with_risk() {
        // Phase 2 tool-surfacing: the context block must surface every
        // catalogued operation, each tagged with its risk.
        let text = catalog_for_context();
        for spec in CATALOG {
            assert!(
                text.contains(spec.name),
                "context catalog missing {}",
                spec.name
            );
        }
        assert!(text.contains("ReadOnly"), "risk tag missing");
        assert!(text.contains("Write"), "risk tag missing");
    }

    #[test]
    fn catalog_lists_all_known_operations() {
        let names: Vec<&str> = CATALOG.iter().map(|spec| spec.name).collect();
        assert!(names.contains(&STDOUT_SEND_TEXT));
        assert!(names.contains(&FEISHU_SEND_MESSAGE));
        assert!(names.contains(&TIME_NOW));
    }

    #[test]
    fn is_allowed_accepts_catalog_operations_only() {
        assert!(is_allowed(STDOUT_SEND_TEXT));
        assert!(is_allowed(FEISHU_SEND_MESSAGE));
        assert!(is_allowed(TIME_NOW));
        assert!(!is_allowed("shell.exec"));
        assert!(!is_allowed(""));
    }

    #[test]
    fn write_operations_are_marked_write_and_time_now_is_read_only() {
        // M2e: `time.now` is the first ReadOnly operation (no side effect,
        // may execute inline without approval). The two reply operations
        // remain Write.
        for spec in CATALOG {
            match spec.name {
                TIME_NOW => assert_eq!(
                    spec.risk,
                    Risk::ReadOnly,
                    "{} should be ReadOnly",
                    spec.name
                ),
                _ => assert_eq!(
                    spec.risk,
                    Risk::Write,
                    "{} should be Write (it produces a side effect)",
                    spec.name
                ),
            }
        }
    }

    #[test]
    fn execution_profile_for_cli_grants_stdout_send_text() {
        // M2b: the CLI channel baseline grant must match the grant the
        // gateway previously hardcoded inline (`stdout.send_text` scoped to
        // the current session), so this change is behavior-preserving.
        let profile = ExecutionProfile::for_channel(ChannelKind::Cli);
        assert_eq!(profile.grants.len(), 1);
        let grant = &profile.grants[0];
        assert_eq!(grant.operation, STDOUT_SEND_TEXT);
        assert_eq!(grant.scope, "current_session");
    }

    #[test]
    fn execution_profile_for_feishu_grants_feishu_send_message() {
        // M2b: the Feishu channel baseline grant must match the grant the
        // gateway previously hardcoded inline (`feishu.send_message` scoped
        // to the current session), so this change is behavior-preserving.
        let profile = ExecutionProfile::for_channel(ChannelKind::Feishu);
        assert_eq!(profile.grants.len(), 1);
        let grant = &profile.grants[0];
        assert_eq!(grant.operation, FEISHU_SEND_MESSAGE);
        assert_eq!(grant.scope, "current_session");
    }

    #[test]
    fn execution_profile_scopes_every_grant_to_current_session() {
        // The baseline profile never grants cross-session scope; a run may
        // only act within its own session. The config-driven follow-up may
        // add grants but must keep (or explicitly widen) this invariant.
        let channels = [(ChannelKind::Cli, "Cli"), (ChannelKind::Feishu, "Feishu")];
        for (channel, name) in channels {
            for grant in ExecutionProfile::for_channel(channel).grants {
                assert_eq!(
                    grant.scope, "current_session",
                    "baseline grant for {name} must be current_session"
                );
            }
        }
    }

    #[test]
    fn with_extra_no_op_when_empty() {
        // Default config (no extra operations) must leave the profile
        // identical to the baseline — behavior preservation gate for M2b's
        // config-driven half.
        let baseline = ExecutionProfile::for_channel(ChannelKind::Cli);
        let augmented = ExecutionProfile::for_channel(ChannelKind::Cli).with_extra(&[]);
        assert_eq!(baseline.grants.len(), augmented.grants.len());
        assert_eq!(augmented.grants[0].operation, STDOUT_SEND_TEXT);
    }

    #[test]
    fn with_extra_adds_catalog_operation() {
        // An operator-configured catalog operation is appended as an extra
        // grant, scoped to current_session.
        let extra = vec![FEISHU_SEND_MESSAGE.to_string()];
        let profile = ExecutionProfile::for_channel(ChannelKind::Cli).with_extra(&extra);
        assert_eq!(profile.grants.len(), 2);
        assert_eq!(profile.grants[0].operation, STDOUT_SEND_TEXT);
        assert_eq!(profile.grants[1].operation, FEISHU_SEND_MESSAGE);
        assert_eq!(profile.grants[1].scope, "current_session");
    }

    #[test]
    fn with_extra_drops_unknown_operations() {
        // Operations not in the catalog cannot be approved by the gateway
        // allowlist, so they are dropped from the grant set rather than
        // appearing as grants that will always be denied.
        let extra = vec![
            "shell.exec".to_string(),
            "".to_string(),
            STDOUT_SEND_TEXT.to_string(), // duplicate of baseline → dropped
        ];
        let profile = ExecutionProfile::for_channel(ChannelKind::Cli).with_extra(&extra);
        assert_eq!(profile.grants.len(), 1);
        assert_eq!(profile.grants[0].operation, STDOUT_SEND_TEXT);
    }
}
