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
        .map_err(|e| anyhow::anyhow!("invalid_manifest: {e}"))?;

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

    // Validate.
    manifest.validate_endpoint()?;
    manifest.validate_operation_name()?;
    manifest.validate_artifact_digest()?;

    // Compute manifest_id.
    let mut manifest_with_id = manifest;
    let manifest_id = manifest_with_id.compute_manifest_id()?;
    manifest_with_id.manifest_id = manifest_id.clone();

    // Register.
    let result = journal.register_harness_manifest(&manifest_with_id)?;

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
        .map_err(|e| anyhow::anyhow!("invalid_request: {e}"))?;

    // Verify manifest exists.
    if journal.load_harness_manifest(&eb.manifest_id)?.is_none() {
        bail!("manifest_not_found");
    }

    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: eb.manifest_id,
        expected_snapshot_id: eb.expected_snapshot_id,
        requested_by: "ipc_operator".into(),
    };

    let approved = gateway.approve_harness_change(intent)?;
    let result = journal.enable_harness(&approved)?;

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
        .map_err(|e| anyhow::anyhow!("invalid_request: {e}"))?;

    // Verify manifest exists.
    if journal.load_harness_manifest(&eb.manifest_id)?.is_none() {
        bail!("manifest_not_found");
    }

    let intent = HarnessChangeIntent {
        action: HarnessChangeAction::Disable,
        manifest_id: eb.manifest_id,
        expected_snapshot_id: eb.expected_snapshot_id,
        requested_by: "ipc_operator".into(),
    };

    let approved = gateway.approve_harness_change(intent)?;
    let result = journal.disable_harness(&approved)?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "previous_snapshot_id": result.previous_snapshot_id,
        "active_snapshot_id": result.active_snapshot_id,
        "changed": result.changed,
    }))?)
}
