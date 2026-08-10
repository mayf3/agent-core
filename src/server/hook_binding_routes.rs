//! Authenticated HookBinding control-plane routes (online bind / unbind).
//!
//! All routes require `Authorization: Bearer <AGENT_CORE_IPC_TOKEN>` (the
//! generic /v1 guard in `server/mod.rs`). They mirror the harness management
//! routes: narrow, typed errors, `expected_snapshot_id` CAS with 409 on
//! conflict, and the audit trail comes from the Journal's transactional
//! activation (RegistrySnapshotActivated event).

use crate::domain::{
    ApprovedHookBindingChange, HookBindingChangeAction, HookBindingChangeIntent,
};
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, HookBinding};
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

/// Narrow, typed error classification for hook binding route handlers.
/// Each variant maps to a single stable HTTP status and a bounded safe
/// error string; no raw validation text, schema, endpoint, token, or
/// anyhow backtrace leaks into the HTTP response.
#[derive(Debug, Clone)]
pub enum HookBindingRouteError {
    InvalidRequest(String),
    Unauthorized,
    SnapshotConflict,
    Internal(String),
}

impl HookBindingRouteError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidRequest(_) => 400,
            Self::Unauthorized => 401,
            Self::SnapshotConflict => 409,
            Self::Internal(_) => 500,
        }
    }

    pub fn safe_error(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::SnapshotConflict => "conflict",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl std::fmt::Display for HookBindingRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::SnapshotConflict => "snapshot_conflict",
            Self::Internal(_) => "internal_error",
        };
        write!(f, "{tag}")
    }
}

impl std::error::Error for HookBindingRouteError {}

#[derive(Deserialize)]
struct BindBody {
    contract: String,
    hook_id: String,
    hook_version: String,
    binding_kind: String,
    binding_key: String,
    provider_id: String,
    endpoint: String,
    expected_snapshot_id: String,
}

#[derive(Deserialize)]
struct UnbindBody {
    contract: String,
    expected_snapshot_id: String,
}

fn parse_binding_kind(kind: &str) -> Result<BindingKind, HookBindingRouteError> {
    match kind {
        "External" => Ok(BindingKind::External),
        "Builtin" => Ok(BindingKind::Builtin),
        _ => Err(HookBindingRouteError::InvalidRequest(format!(
            "binding_kind must be External or Builtin, got {kind:?}"
        ))),
    }
}

pub fn handle_bind(
    gateway: &Gateway,
    journal: &JournalStore,
    body: &serde_json::Value,
) -> Result<String> {
    let bb: BindBody = serde_json::from_value(body.clone())
        .map_err(|e| HookBindingRouteError::InvalidRequest(format!("{e}")))?;
    let binding_kind = parse_binding_kind(&bb.binding_kind)?;
    let binding = HookBinding {
        contract: bb.contract.clone(),
        hook_id: bb.hook_id,
        hook_version: bb.hook_version,
        binding_kind,
        binding_key: bb.binding_key,
        provider_id: bb.provider_id,
        endpoint: bb.endpoint,
    };
    let intent = HookBindingChangeIntent {
        action: HookBindingChangeAction::Bind,
        contract: bb.contract,
        binding: Some(binding),
        expected_snapshot_id: bb.expected_snapshot_id,
        requested_by: "ipc_operator".into(),
    };
    let approved: ApprovedHookBindingChange = gateway.approve_hook_binding_change(intent)?;
    let result = journal.bind_hook(&approved).map_err(map_journal_error)?;
    Ok(serde_json::to_string(&json!({
        "ok": true,
        "previous_snapshot_id": result.previous_snapshot_id,
        "active_snapshot_id": result.active_snapshot_id,
        "changed": result.changed,
    }))?)
}

pub fn handle_unbind(
    gateway: &Gateway,
    journal: &JournalStore,
    body: &serde_json::Value,
) -> Result<String> {
    let ub: UnbindBody = serde_json::from_value(body.clone())
        .map_err(|e| HookBindingRouteError::InvalidRequest(format!("{e}")))?;
    let intent = HookBindingChangeIntent {
        action: HookBindingChangeAction::Unbind,
        contract: ub.contract,
        binding: None,
        expected_snapshot_id: ub.expected_snapshot_id,
        requested_by: "ipc_operator".into(),
    };
    let approved: ApprovedHookBindingChange = gateway.approve_hook_binding_change(intent)?;
    let result = journal.unbind_hook(&approved).map_err(map_journal_error)?;
    Ok(serde_json::to_string(&json!({
        "ok": true,
        "previous_snapshot_id": result.previous_snapshot_id,
        "active_snapshot_id": result.active_snapshot_id,
        "changed": result.changed,
    }))?)
}

fn map_journal_error(e: anyhow::Error) -> HookBindingRouteError {
    let msg = e.to_string();
    if msg.starts_with("snapshot_conflict") {
        HookBindingRouteError::SnapshotConflict
    } else if msg.starts_with("invalid_hook_binding") {
        HookBindingRouteError::InvalidRequest(msg)
    } else {
        HookBindingRouteError::Internal(msg)
    }
}

/// Map a `Result<String>` from a hook binding route handler to an HTTP
/// response on the given stream.
pub fn handle_hook_binding_result(
    stream: &mut std::net::TcpStream,
    result: Result<String>,
) -> Result<()> {
    match result {
        Ok(body) => super::write_json(stream, 200, serde_json::from_str(&body)?),
        Err(e) => {
            let (status, safe_msg) = if let Some(hb_err) = e.downcast_ref::<HookBindingRouteError>()
            {
                (hb_err.http_status(), hb_err.safe_error())
            } else {
                let msg = e.to_string();
                if msg.starts_with("invalid_") {
                    (400, "invalid_request")
                } else if msg.starts_with("snapshot_conflict") {
                    (409, "conflict")
                } else if msg.starts_with("unauthorized") {
                    (401, "unauthorized")
                } else {
                    (500, "internal_error")
                }
            };
            super::write_json(stream, status, json!({ "ok": false, "error": safe_msg }))
        }
    }
}
