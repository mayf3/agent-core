//! Fixed policy pipeline for invocation approval (Phase 2 M2c).
//!
//! Previously `Gateway::approve_invocation` ran the access-control checks as
//! an inline ladder of `bail!` calls: grant presence → catalog allowlist →
//! session-scope. This module lifts them into an ordered, pure
//! [`evaluate_policy`] function that returns a [`PolicyVerdict`] without any
//! I/O or `Gateway` state. The gateway calls it and maps `Deny` back to an
//! error, so the externally surfaced error messages are unchanged.
//!
//! Keeping the pipeline pure and ordered means a future increment can add
//! stages (e.g. a deny-list, an argument transform) without touching
//! `approve_invocation`'s wiring, and the pipeline is unit-testable in
//! isolation.
//!
//! See `docs/decisions/phase2-invocation-gateway-scoping.md` (M2c).

use crate::domain::operation;
use crate::domain::{InvocationIntent, Run, Session};

/// The verdict of the invocation policy pipeline.
///
/// `Allow` means the intent passes access control; `Deny` carries the same
/// sanitized reason string the gateway previously `bail!`ed inline. A
/// `Transform(intent)` stage is deliberately omitted for now — no current
/// policy rewrites an intent, and the scoping doc lists it as a future
/// extension rather than a present requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// The intent is approved by the policy pipeline.
    Allow,
    /// The intent is denied; `reason` is a sanitized, log-safe string
    /// (never echoes credentials), identical to the message the gateway
    /// previously surfaced.
    Deny(String),
}

impl PolicyVerdict {
    /// Convenience: a `Deny` verdict.
    pub fn deny(reason: impl Into<String>) -> Self {
        PolicyVerdict::Deny(reason.into())
    }

    /// True when this is an `Allow`.
    pub fn is_allow(&self) -> bool {
        matches!(self, PolicyVerdict::Allow)
    }
}

/// Evaluate the fixed invocation policy pipeline against an intent, run, and
/// session. Pure: no I/O, no `Gateway` state, no mutation.
///
/// Stages run in this order (first denial wins, matching the previous inline
/// ladder exactly):
///
/// 1. **Grant** — the run's principal must hold a capability grant for the
///    operation; else `capability_not_enabled: {op}`.
/// 2. **Catalog** — the operation must be in the operation catalog; else
///    `operation_not_allowed: {op}`.
/// 3. **Session scope** — the intent's `session_id` argument must equal the
///    run's session; else `target_session_mismatch`.
///
/// Argument *shape* validation (e.g. feishu requiring message_id/chat_id/text)
/// is intentionally **not** part of the access pipeline — it is a schema
/// concern (M2a's `argument_schema`, deferred) and stays in
/// `approve_invocation`, where it produces `missing string argument: {key}`.
pub fn evaluate_policy(intent: &InvocationIntent, run: &Run, session: &Session) -> PolicyVerdict {
    // Stage 1: capability grant.
    let has_grant = run
        .principal
        .grants
        .iter()
        .any(|grant| grant.operation == intent.operation);
    if !has_grant {
        return PolicyVerdict::deny(format!("capability_not_enabled: {}", intent.operation));
    }

    // Stage 2: operation catalog allowlist (single source of truth, M2a).
    if !operation::is_allowed(&intent.operation) {
        return PolicyVerdict::deny(format!("operation_not_allowed: {}", intent.operation));
    }

    // Stage 3: session scope. A missing/empty session_id is a mismatch — the
    // intent must target the run's own session. `string_arg`-style parsing is
    // the gateway's job; here we compare the raw value so the pipeline stays
    // free of error-formatting concerns.
    let target_session = intent
        .arguments
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if target_session != session.id.0 {
        return PolicyVerdict::deny("target_session_mismatch");
    }

    PolicyVerdict::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use chrono::Utc;
    use serde_json::json;

    fn session_with_id(id: &str) -> Session {
        Session {
            id: SessionId(id.to_string()),
            agent_id: AgentId("main".to_string()),
            channel: ChannelKind::Cli,
            conversation_key: "local".to_string(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        }
    }

    fn run_with_grants(grants: Vec<CapabilityGrant>) -> Run {
        Run {
            id: RunId::new(),
            session_id: SessionId("session_current".to_string()),
            agent_id: AgentId("main".to_string()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".to_string()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants,
                requester_id: Some("cli:local".to_string()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn intent(operation: &str, session_id: &str) -> InvocationIntent {
        InvocationIntent {
            invocation_id: InvocationId::new(),
            run_id: RunId::new(),
            operation: operation.to_string(),
            arguments: json!({ "session_id": session_id }),
            idempotency_key: None,
        }
    }

    #[test]
    fn allows_when_grant_catalog_and_session_all_match() {
        let session = session_with_id("session_current");
        let run = run_with_grants(vec![CapabilityGrant {
            operation: operation::STDOUT_SEND_TEXT.to_string(),
            scope: "current_session".to_string(),
        }]);
        let intent = intent(operation::STDOUT_SEND_TEXT, "session_current");
        assert_eq!(
            evaluate_policy(&intent, &run, &session),
            PolicyVerdict::Allow
        );
    }

    #[test]
    fn denies_when_principal_lacks_grant() {
        let session = session_with_id("session_current");
        let run = run_with_grants(vec![]);
        let intent = intent(operation::STDOUT_SEND_TEXT, "session_current");
        assert_eq!(
            evaluate_policy(&intent, &run, &session),
            PolicyVerdict::deny("capability_not_enabled: stdout.send_text")
        );
    }

    #[test]
    fn denies_when_operation_not_in_catalog() {
        // The principal "holds" a grant for an op that is not catalogued. The
        // catalog stage must still deny it (grant ≠ allowlist).
        let session = session_with_id("session_current");
        let run = run_with_grants(vec![CapabilityGrant {
            operation: "shell.exec".to_string(),
            scope: "current_session".to_string(),
        }]);
        let intent = intent("shell.exec", "session_current");
        assert_eq!(
            evaluate_policy(&intent, &run, &session),
            PolicyVerdict::deny("operation_not_allowed: shell.exec")
        );
    }

    #[test]
    fn denies_on_session_mismatch() {
        let session = session_with_id("session_current");
        let run = run_with_grants(vec![CapabilityGrant {
            operation: operation::STDOUT_SEND_TEXT.to_string(),
            scope: "current_session".to_string(),
        }]);
        let intent = intent(operation::STDOUT_SEND_TEXT, "session_other");
        assert_eq!(
            evaluate_policy(&intent, &run, &session),
            PolicyVerdict::deny("target_session_mismatch")
        );
    }

    #[test]
    fn denies_on_missing_session_argument() {
        // A missing session_id must be treated as a mismatch, not an allow —
        // the intent may never act outside (or ambiguously regarding) its
        // session.
        let session = session_with_id("session_current");
        let run = run_with_grants(vec![CapabilityGrant {
            operation: operation::STDOUT_SEND_TEXT.to_string(),
            scope: "current_session".to_string(),
        }]);
        let intent = InvocationIntent {
            invocation_id: InvocationId::new(),
            run_id: RunId::new(),
            operation: operation::STDOUT_SEND_TEXT.to_string(),
            arguments: json!({}),
            idempotency_key: None,
        };
        assert_eq!(
            evaluate_policy(&intent, &run, &session),
            PolicyVerdict::deny("target_session_mismatch")
        );
    }

    #[test]
    fn pipeline_is_pure_no_state_required() {
        // evaluate_policy takes only borrowed domain values; it reads no
        // Gateway/config. Calling it twice with the same inputs is stable.
        let session = session_with_id("s");
        let run = run_with_grants(vec![CapabilityGrant {
            operation: operation::FEISHU_SEND_MESSAGE.to_string(),
            scope: "current_session".to_string(),
        }]);
        let intent = intent(operation::FEISHU_SEND_MESSAGE, "s");
        let first = evaluate_policy(&intent, &run, &session);
        let second = evaluate_policy(&intent, &run, &session);
        assert_eq!(first, second);
        assert!(first.is_allow());
    }
}
