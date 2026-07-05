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

// Re-export coding operation names and risk classification so existing
// import paths (crate::domain::operation::external::*, etc.) continue to work.
pub use super::coding_operations::*;

/// The risk classification of an operation. `Write` operations use the
/// approval/dispatch boundary; catalogued `ReadOnly` operations may execute
/// inline after the Gateway approves the current run's explicit grant.
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
        name: SESSION_RECALL_RECENT,
        risk: Risk::ReadOnly,
    },
    OperationSpec {
        name: SYSTEM_STATUS,
        risk: Risk::ReadOnly,
    },
];

pub const STDOUT_SEND_TEXT: &str = "stdout.send_text";
pub const FEISHU_SEND_MESSAGE: &str = "feishu.send_message";
/// Read-only: recall recent messages from the **current session only**.
/// Returns normalized text/role/event_id/created_at — never raw payload JSON,
/// Authorization, tokens, or cross-session data. Implemented inline in the
/// Runtime via `JournalStore::recent_user_messages`. The first *practical*
/// read-only tool: lets the agent recall earlier context the user mentioned.
pub const SESSION_RECALL_RECENT: &str = "session.recall_recent";
/// Read-only: return a snapshot of system health and projection state.
/// Returns aggregate journal counts (outbox, ingress, hash chain) — never
/// secrets, payloads, tokens, or raw event content. Implemented inline via
/// `execute_system_status` in the Runtime.
pub const SYSTEM_STATUS: &str = "system.status";

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

/// The minimal OpenAI-compatible tool definition for a catalogued `ReadOnly`
/// operation exposed to the model. Returns `None` for `Write` operations or
/// unknown names — Write operations are NEVER sent to the provider as tools.
///
/// This is the single closed mapping from a catalog operation to its provider
/// schema; there is no second hand-maintained tool list in `llm/mod.rs`. Each
/// schema is strict (`additionalProperties: false`). The Gateway remains the
/// final authorization boundary — schema exposure is only a prompt hint.
pub fn provider_tool_definition(name: &str) -> Option<serde_json::Value> {
    use serde_json::json;
    let spec = lookup(name)?;
    if spec.risk != Risk::ReadOnly {
        return None;
    }
    let (description, parameters) = match spec.name {
        SESSION_RECALL_RECENT => (
            "Recall recent messages from the current session (read-only, current session only).",
            json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Max messages to recall (default 5)." },
                    "query": { "type": "string", "description": "Optional case-insensitive substring filter." }
                },
                "required": [],
                "additionalProperties": false
            }),
        ),
        SYSTEM_STATUS => (
            "Return system health and projection summary (aggregate counts only, no secrets).",
            json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }),
        ),
        _ => return None,
    };
    Some(json!({
        "type": "function",
        "function": {
            "name": spec.name,
            "description": description,
            "parameters": parameters,
        }
    }))
}

/// Build the OpenAI-compatible `tools` array for the provider request from the
/// agent's granted operations: a tool is exposed ONLY when it is both granted
/// to the run principal AND a catalogued `ReadOnly` operation. Write operations
/// and unknown names are never included. Order is the catalog order (stable).
pub fn provider_tools_for_grants(granted_operations: &[String]) -> Vec<serde_json::Value> {
    CATALOG
        .iter()
        .filter(|spec| {
            spec.risk == Risk::ReadOnly && granted_operations.iter().any(|g| g == spec.name)
        })
        .filter_map(|spec| provider_tool_definition(spec.name))
        .collect()
}

/// Build the ToolCatalog text for the model from the **current Run's grants**:
/// a tool is listed only when it is BOTH granted to the run principal AND a
/// catalogued `ReadOnly` operation. Write operations and unknown names are
/// never listed. Order is the catalog order (stable, deduplicated).
///
/// This stays consistent with `provider_tools_for_grants` — the operation set
/// shown to the model in the ToolCatalog block equals the set in the provider
/// `tools` schema. The Gateway remains the independent final authorization
/// boundary; surfacing is only a prompt hint.
pub fn catalog_for_context_grants(granted_operations: &[String]) -> String {
    let names: Vec<&str> = CATALOG
        .iter()
        .filter(|spec| {
            spec.risk == Risk::ReadOnly && granted_operations.iter().any(|g| g == spec.name)
        })
        .map(|spec| spec.name)
        .collect();
    if names.is_empty() {
        return "No tools are available for this request.".to_string();
    }
    let rows = names
        .into_iter()
        .map(|name| {
            let desc = match name {
                SESSION_RECALL_RECENT => {
                    "recall recent messages from the current session (read-only, current session only)."
                }
                SYSTEM_STATUS => "read system health and projection summary (aggregate counts only).",
                _ => "catalogued read-only operation.",
            };
            format!("{name} - {desc}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("Available tools (authorized for this request, read-only):\n{rows}")
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
        let reply_operation = match channel {
            ChannelKind::Cli => STDOUT_SEND_TEXT,
            ChannelKind::Feishu => FEISHU_SEND_MESSAGE,
        };
        Self {
            grants: vec![
                CapabilityGrant {
                    operation: reply_operation.to_string(),
                    scope: "current_session".to_string(),
                },
                // Read-only tools available on every channel: the agent may
                // recall messages from its own session without approval.
                CapabilityGrant {
                    operation: SESSION_RECALL_RECENT.to_string(),
                    scope: "current_session".to_string(),
                },
                // NOTE: system.status is NOT granted here. It is added via
                // KernelConfig.extra_allowed_operations in the default config.
                // This ensures the grant is a per-Agent configuration choice,
                // not a channel-level permission. Future agents must explicitly
                // configure the grant via extra_allowed_operations.
            ],
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
/// propose.
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
                SESSION_RECALL_RECENT => "recall recent messages from the current session (read-only, current session only, no cross-session access).",
                SYSTEM_STATUS => "read system health and projection state (aggregate journal counts only, no secrets or payloads).",
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
        assert!(names.contains(&SESSION_RECALL_RECENT));
        assert!(names.contains(&SYSTEM_STATUS));
        assert!(!names.contains(&"time.now"));
    }

    #[test]
    fn is_allowed_accepts_catalog_operations_only() {
        assert!(is_allowed(STDOUT_SEND_TEXT));
        assert!(is_allowed(FEISHU_SEND_MESSAGE));
        assert!(is_allowed(SESSION_RECALL_RECENT));
        assert!(is_allowed(FEISHU_SEND_MESSAGE));
        assert!(!is_allowed("time.now"));
        assert!(!is_allowed("shell.exec"));
        assert!(!is_allowed(""));
    }

    #[test]
    fn write_operations_are_marked_write_and_read_only_ops_are_read_only() {
        // ReadOnly operations: `session.recall_recent` + `system.status` (no side
        // effect, may execute inline). The two reply operations are Write.
        for spec in CATALOG {
            match spec.name {
                SESSION_RECALL_RECENT | SYSTEM_STATUS => assert_eq!(
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
    fn execution_profile_for_cli_grants_stdout_send_text_and_recall() {
        // The CLI channel baseline includes the reply operation + the
        // session.recall_recent read-only tool (scoped to current session).
        // system.status and time.now are NOT in the baseline — system.status is
        // granted via extra_allowed_operations in the default config (see
        // config.rs); time.now is retired and only available via external harness.
        let profile = ExecutionProfile::for_channel(ChannelKind::Cli);
        assert_eq!(profile.grants.len(), 2);
        let ops: Vec<&str> = profile
            .grants
            .iter()
            .map(|g| g.operation.as_str())
            .collect();
        assert!(ops.contains(&STDOUT_SEND_TEXT));
        assert!(ops.contains(&SESSION_RECALL_RECENT));
        assert!(
            !ops.contains(&SYSTEM_STATUS),
            "system.status is NOT a channel-level grant"
        );
        for grant in &profile.grants {
            assert_eq!(grant.scope, "current_session");
        }
    }

    #[test]
    fn execution_profile_for_feishu_grants_feishu_send_message_and_recall() {
        let profile = ExecutionProfile::for_channel(ChannelKind::Feishu);
        assert_eq!(profile.grants.len(), 2);
        let ops: Vec<&str> = profile
            .grants
            .iter()
            .map(|g| g.operation.as_str())
            .collect();
        assert!(ops.contains(&FEISHU_SEND_MESSAGE));
        assert!(ops.contains(&SESSION_RECALL_RECENT));
        assert!(
            !ops.contains(&SYSTEM_STATUS),
            "system.status is NOT a channel-level grant"
        );
        for grant in &profile.grants {
            assert_eq!(grant.scope, "current_session");
        }
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
        // No extra operations added by empty config.
        assert!(!augmented
            .grants
            .iter()
            .any(|g| g.operation == FEISHU_SEND_MESSAGE));
    }

    #[test]
    fn with_extra_adds_catalog_operation() {
        // An operator-configured catalog operation is appended as an extra
        // grant, scoped to current_session.
        let extra = vec![FEISHU_SEND_MESSAGE.to_string()];
        let profile = ExecutionProfile::for_channel(ChannelKind::Cli).with_extra(&extra);
        // Baseline is 2 (reply + recall), + 1 extra = 3.
        assert_eq!(profile.grants.len(), 3);
        assert_eq!(profile.grants[0].operation, STDOUT_SEND_TEXT);
        assert_eq!(profile.grants[2].operation, FEISHU_SEND_MESSAGE);
        assert_eq!(profile.grants[2].scope, "current_session");
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
        // Baseline is 2 (stdout + recall); the extra STDOUT_SEND_TEXT is a
        // duplicate (dropped), unknown/empty dropped → still 2.
        assert_eq!(profile.grants.len(), 2);
        assert_eq!(profile.grants[0].operation, STDOUT_SEND_TEXT);
    }
}
