//! Typed tool-call rejection categories and bounded audit sanitization.
//!
//! The Gateway produces typed rejection values; this module owns bounded audit
//! sanitization, argument validation, and capability execution helpers.
//!
//! This is a minimal, tool-loop-boundary-only enum — deliberately NOT a general
//! error framework.

use crate::domain::operation;
use crate::domain::{ApprovedInvocation, ReceiptStatus, SessionId};
use crate::gateway::ToolRejection;
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, OperationSpec, RegistrySnapshot};
use anyhow::Result;
use serde_json::json;
use sha2::{Digest, Sha256};

/// Sanitize an untrusted operation name for Journal audit. Known operations
/// (from the static catalog OR from a pinned RegistrySnapshot) record their
/// canonical name; anything else collapses to the fixed `"unknown_operation"`
/// plus a fixed-length correlation digest.
///
/// The raw operation string is NEVER written to the Journal — it may be
/// arbitrarily long, contain unicode/control/path characters, or resemble a
/// token/authorization value. The digest is 8 hex chars (32 bits) and exists
/// only so two distinct unknown operations remain distinguishable in audit; it
/// is not reversible to the input and carries no sensitive content.
pub(crate) fn sanitize_operation_for_audit(op: &str) -> String {
    if let Some(spec) = operation::lookup(op) {
        return spec.name.to_string();
    }
    let digest = operation_digest(op);
    format!("unknown_operation_{digest}")
}

/// Snapshot-aware variant: checks the static catalog AND the pinned snapshot.
pub(crate) fn sanitize_operation_for_audit_with_snapshot(
    op: &str,
    snapshot: &RegistrySnapshot,
) -> String {
    // Known from static catalog.
    if let Some(spec) = operation::lookup(op) {
        return spec.name.to_string();
    }
    // Known from pinned snapshot.
    if snapshot.lookup(op).is_some() {
        return op.to_string();
    }
    let digest = operation_digest(op);
    format!("unknown_operation_{digest}")
}

/// 8-hex-char (32-bit) digest of an arbitrary operation string. Used only as a
/// fixed-length correlation suffix for unknown operations; never written raw.
fn operation_digest(op: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(op.as_bytes());
    hex::encode(&hasher.finalize()[..4])
}

/// Derive a stable, bounded, internal audit identifier for a tool call from
/// trusted call-position values only. Used for malformed tool calls where no
/// provider id is available/usable. The raw provider payload is never part of
/// this id.
///
/// Composition: `tc:{run_id_short}:{turn_index}:{tool_index}` where
/// `run_id_short` is the first 8 chars of the run id. All components are
/// trusted/bounded, so the result is bounded and stable for the same
/// (run, turn, index) triple.
pub(crate) fn internal_tool_call_id(run_id: &str, turn_index: usize, tool_index: usize) -> String {
    let short = run_id.chars().take(12).collect::<String>();
    format!("tc:{short}:{turn_index}:{tool_index}")
}

/// Execute the `session.recall_recent` capability: recall recent user messages
/// from the current session only, with a bounded limit and per-message
/// truncation. Returns normalized text/role/event_id — never raw payload JSON,
/// Authorization, tokens, or cross-session data. Uses the fault-aware recall
/// entry point so a deterministic test-only fault can be injected precisely at
/// the capability boundary while the rest of the Journal stays writable.
pub(crate) fn execute_session_recall(
    journal: &JournalStore,
    session_id: &SessionId,
    approved: &ApprovedInvocation,
) -> Result<(ReceiptStatus, serde_json::Value, String)> {
    const MAX_RECALL_LIMIT: usize = 20;
    const MAX_RECALL_CHARS: usize = 500;

    let args = &approved.intent().arguments;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n.clamp(1, MAX_RECALL_LIMIT as u64) as usize)
        .unwrap_or(5);
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let messages = journal.recent_user_messages_for_capability(session_id, limit)?;

    let mut results: Vec<serde_json::Value> = Vec::new();
    for (event_id, text) in &messages {
        if let Some(ref q) = query {
            if !text.to_lowercase().contains(q) {
                continue;
            }
        }
        let truncated: String = text.chars().take(MAX_RECALL_CHARS).collect();
        results.push(json!({
            "event_id": event_id,
            "role": "user",
            "text": truncated,
        }));
    }

    let output = json!({
        "session_id": session_id.0,
        "count": results.len(),
        "messages": results,
    });

    let text = if results.is_empty() {
        "no matching messages found".to_string()
    } else {
        results
            .iter()
            .filter_map(|m| m.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    Ok((ReceiptStatus::Succeeded, output, text))
}

/// Schema validation of model arguments against an OperationSpec.
/// Builtin operations use the existing hardcoded rules; External operations
/// use the spec's JSON Schema parameters.
pub fn validate_model_arguments(
    spec: &OperationSpec,
    arguments: &serde_json::Value,
) -> Result<(), ToolRejection> {
    match spec.binding_kind {
        BindingKind::Builtin => validate_builtin_arguments(spec.name.as_str(), arguments),
        BindingKind::External => {
            crate::registry::schema::validate_against_schema(&spec.parameters, arguments)
                .map_err(|_| ToolRejection::InvalidArguments)
        }
    }
}

/// Builtin-specific argument validation (same rules as before).
fn validate_builtin_arguments(
    operation: &str,
    arguments: &serde_json::Value,
) -> Result<(), ToolRejection> {
    let Some(map) = arguments.as_object() else {
        return Err(ToolRejection::MalformedArguments);
    };
    match operation {
        operation::SYSTEM_STATUS => {
            if !map.is_empty() {
                return Err(ToolRejection::InvalidArguments);
            }
        }
        operation::SESSION_RECALL_RECENT => {
            for (key, value) in map {
                match key.as_str() {
                    "limit" => {
                        let Some(n) = value.as_u64() else {
                            return Err(ToolRejection::InvalidArguments);
                        };
                        if !(1..=20).contains(&n) {
                            return Err(ToolRejection::InvalidArguments);
                        }
                    }
                    "query" => {
                        if !value.is_string() {
                            return Err(ToolRejection::InvalidArguments);
                        }
                    }
                    _ => return Err(ToolRejection::InvalidArguments),
                }
            }
        }
        _ => return Err(ToolRejection::UnknownOperation),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_and_message_are_fixed_and_bounded() {
        for variant in [
            ToolRejection::UnknownOperation,
            ToolRejection::OperationNotAllowed,
            ToolRejection::MalformedArguments,
            ToolRejection::InvalidArguments,
            ToolRejection::PolicyDenied,
            ToolRejection::MalformedToolCall,
            ToolRejection::InternalToolError,
        ] {
            let cat = variant.category();
            let msg = variant.safe_message();
            assert!(
                !cat.is_empty() && cat.len() <= 32,
                "category bounded: {cat}"
            );
            assert!(!msg.is_empty() && msg.len() <= 80, "message bounded: {msg}");
            // Categories are snake_case identifiers, never raw error text.
            assert!(
                cat.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "category is a fixed identifier: {cat}"
            );
        }
    }

    #[test]
    fn sanitize_keeps_catalog_operation_canonical() {
        assert_eq!(sanitize_operation_for_audit("time.now"), "time.now");
        assert_eq!(
            sanitize_operation_for_audit("session.recall_recent"),
            "session.recall_recent"
        );
        assert_eq!(
            sanitize_operation_for_audit("system.status"),
            "system.status"
        );
    }

    #[test]
    fn sanitize_collapses_unknown_to_fixed_prefix_with_digest() {
        let s = sanitize_operation_for_audit("shell.exec");
        assert!(
            s.starts_with("unknown_operation_"),
            "unknown op collapses to fixed prefix: {s}"
        );
        // 8 hex digest chars after the prefix.
        let suffix = s.strip_prefix("unknown_operation_").unwrap();
        assert_eq!(suffix.len(), 8, "digest is 8 hex chars: {suffix}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "digest is hex: {suffix}"
        );
    }

    #[test]
    fn sanitize_distinct_unknowns_have_distinct_digests() {
        let a = sanitize_operation_for_audit("shell.exec");
        let b = sanitize_operation_for_audit("rm -rf /");
        assert_ne!(a, b, "distinct unknown ops must differ");
    }

    #[test]
    fn sanitize_is_deterministic() {
        assert_eq!(
            sanitize_operation_for_audit("shell.exec"),
            sanitize_operation_for_audit("shell.exec")
        );
    }

    #[test]
    fn sanitize_never_leaks_raw_untrusted_input() {
        // Over-long, unicode, control/path chars, token-like text — none leak.
        let cases = [
            "x".repeat(10_000),
            "操作🔥with emoji\nand\tcontrol/path\\chars".to_string(),
            "credential_marker_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890".to_string(),
            "header_marker_supersecretpassword".to_string(),
            "../../../etc/passwd".to_string(),
        ];
        for op in &cases {
            let s = sanitize_operation_for_audit(op);
            assert!(!s.contains(op.as_str()), "raw op leaked: {s}");
            assert!(
                !s.contains("credential_marker")
                    && !s.contains("header_marker")
                    && !s.contains("passwd"),
                "token-like content leaked for {op}: {s}"
            );
        }
    }

    #[test]
    fn internal_id_is_bounded_and_stable() {
        let a = internal_tool_call_id("run_abc123def456", 0, 0);
        let b = internal_tool_call_id("run_abc123def456", 0, 0);
        assert_eq!(a, b, "stable for same (run,turn,index)");
        assert!(
            a.starts_with("tc:run_abc123"),
            "starts with short run prefix"
        );
        // Distinct turn → distinct id.
        assert_ne!(
            internal_tool_call_id("run_abc123def456", 0, 0),
            internal_tool_call_id("run_abc123def456", 1, 0)
        );
        // Distinct index → distinct id.
        assert_ne!(
            internal_tool_call_id("run_abc123def456", 0, 0),
            internal_tool_call_id("run_abc123def456", 0, 1)
        );
        // Distinct run → distinct id.
        assert_ne!(
            internal_tool_call_id("run_abc123def456", 0, 0),
            internal_tool_call_id("run_zzz999zzz888", 0, 0)
        );
        // Bounded length even for a very long run id.
        let long = internal_tool_call_id(&"x".repeat(10_000), 999, 999);
        assert!(
            long.len() < 60,
            "internal id is bounded: {} chars",
            long.len()
        );
    }
}
