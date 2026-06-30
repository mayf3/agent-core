//! Authenticated harness management API routes.
//!
//! All routes require `Authorization: Bearer <AGENT_CORE_IPC_TOKEN>`.
//! These are narrow, authenticated operations — not a general control surface.

use crate::gateway::Gateway;
use crate::harness::control::HarnessChangeAction;
use crate::harness::control::HarnessChangeIntent;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

/// Narrow, typed error classification for harness route handlers.
/// Each variant maps to a single stable HTTP status and a bounded
/// safe error string; no raw validation text, schema, endpoint,
/// token, or anyhow backtrace leaks into the HTTP response.
#[derive(Debug, Clone)]
pub enum HarnessRouteError {
    InvalidManifest(String),
    InvalidRequest(String),
    Unauthorized,
    ManifestNotFound,
    SnapshotConflict,
    Internal(String),
}

impl HarnessRouteError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidManifest(_) => 400,
            Self::InvalidRequest(_) => 400,
            Self::Unauthorized => 401,
            Self::ManifestNotFound => 404,
            Self::SnapshotConflict => 409,
            Self::Internal(_) => 500,
        }
    }

    pub fn safe_error(&self) -> &'static str {
        match self {
            Self::InvalidManifest(_) | Self::InvalidRequest(_) => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::ManifestNotFound => "not_found",
            Self::SnapshotConflict => "conflict",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl std::fmt::Display for HarnessRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self {
            Self::InvalidManifest(_) => "invalid_manifest",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::ManifestNotFound => "manifest_not_found",
            Self::SnapshotConflict => "snapshot_conflict",
            Self::Internal(_) => "internal_error",
        };
        write!(f, "{tag}")
    }
}

impl std::error::Error for HarnessRouteError {}

// HarnessRouteError: Send + Sync + 'static so anyhow's blanket impl
// auto-converts it via `?` and `anyhow::Error::from`.

#[derive(Deserialize)]
struct RegisterBody {
    harness_id: String,
    artifact_digest: String,
    protocol_version: String,
    endpoint: String,
    operation_name: String,
    description: String,
    input_schema: serde_json::Value,
    output_schema: serde_json::Value,
    idempotent: bool,
}

#[derive(Deserialize)]
struct EnableDisableBody {
    manifest_id: String,
    expected_snapshot_id: String,
}

pub fn handle_register(
    _gateway: &Gateway,
    journal: &JournalStore,
    body: &serde_json::Value,
) -> Result<String> {
    let reg: RegisterBody = serde_json::from_value(body.clone())
        .map_err(|e| HarnessRouteError::InvalidRequest(format!("{e}")))?;

    // Validate fields.
    let manifest = HarnessManifest {
        manifest_id: String::new(), // will be computed
        harness_id: reg.harness_id,
        artifact_digest: reg.artifact_digest,
        protocol_version: reg.protocol_version,
        endpoint: reg.endpoint,
        operation_name: reg.operation_name,
        description: reg.description,
        input_schema: reg.input_schema,
        output_schema: reg.output_schema,
        idempotent: reg.idempotent,
        created_at: Utc::now(),
    };

    // Validate all fields through the typed error path.
    manifest
        .validate_all()
        .map_err(|e| HarnessRouteError::InvalidManifest(format!("{e}")))?;

    // Validate schemas can be parsed by strict validator.
    crate::registry::schema::validate_schema_structure(&manifest.input_schema)
        .map_err(|e| HarnessRouteError::InvalidManifest(format!("invalid input_schema: {e}")))?;
    crate::registry::schema::validate_schema_structure(&manifest.output_schema)
        .map_err(|e| HarnessRouteError::InvalidManifest(format!("invalid output_schema: {e}")))?;

    // Compute manifest_id.
    let mut manifest_with_id = manifest;
    let manifest_id = manifest_with_id.compute_manifest_id()?;
    manifest_with_id.manifest_id = manifest_id.clone();

    // Register.
    let result = journal
        .register_harness_manifest(&manifest_with_id)
        .map_err(|e| HarnessRouteError::Internal(format!("{e}")))?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "manifest_id": result,
    }))?)
}

pub fn handle_enable(
    gateway: &Gateway,
    journal: &JournalStore,
    body: &serde_json::Value,
) -> Result<String> {
    let eb: EnableDisableBody = serde_json::from_value(body.clone())
        .map_err(|e| HarnessRouteError::InvalidRequest(format!("{e}")))?;

    // Verify manifest exists.
    if journal.load_harness_manifest(&eb.manifest_id)?.is_none() {
        return Err(HarnessRouteError::ManifestNotFound.into());
    }

    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: eb.manifest_id,
        expected_snapshot_id: eb.expected_snapshot_id,
        requested_by: "ipc_operator".into(),
    };

    let approved = gateway.approve_harness_change(intent)?;
    let result = journal.enable_harness(&approved).map_err(|e| {
        let msg = e.to_string();
        if msg.starts_with("snapshot_conflict") {
            HarnessRouteError::SnapshotConflict
        } else {
            HarnessRouteError::Internal(format!("{e}"))
        }
    })?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "previous_snapshot_id": result.previous_snapshot_id,
        "active_snapshot_id": result.active_snapshot_id,
        "changed": result.changed,
    }))?)
}

pub fn handle_disable(
    gateway: &Gateway,
    journal: &JournalStore,
    body: &serde_json::Value,
) -> Result<String> {
    let eb: EnableDisableBody = serde_json::from_value(body.clone())
        .map_err(|e| HarnessRouteError::InvalidRequest(format!("{e}")))?;

    // Verify manifest exists.
    if journal.load_harness_manifest(&eb.manifest_id)?.is_none() {
        return Err(HarnessRouteError::ManifestNotFound.into());
    }

    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Disable,
        manifest_id: eb.manifest_id,
        expected_snapshot_id: eb.expected_snapshot_id,
        requested_by: "ipc_operator".into(),
    };

    let approved = gateway.approve_harness_change(intent)?;
    let result = journal.disable_harness(&approved).map_err(|e| {
        let msg = e.to_string();
        if msg.starts_with("snapshot_conflict") {
            HarnessRouteError::SnapshotConflict
        } else {
            HarnessRouteError::Internal(format!("{e}"))
        }
    })?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "previous_snapshot_id": result.previous_snapshot_id,
        "active_snapshot_id": result.active_snapshot_id,
        "changed": result.changed,
    }))?)
}
