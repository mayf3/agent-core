//! Admin HTTP handlers for the harness control plane.
//!
//! Only available when `AGENT_CORE_HARNESS_ADMIN_TOKEN` is configured.
//! All routes require `Authorization: Bearer <admin_token>`.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::harness::grants::{self};
use crate::harness::manifest::{self, HarnessBundleManifest, PreparedOperation};
use crate::harness::registration::{self};
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use serde_json::Value;

/// Check if the admin token is configured.
pub fn is_admin_enabled(config: &KernelConfig) -> bool {
    !config.harness_admin_token.is_empty()
}

/// Validate the bearer token against the configured admin token.
pub fn validate_admin_token(config: &KernelConfig, token: Option<&str>) -> Result<()> {
    if config.harness_admin_token.is_empty() {
        bail!("harness admin not configured");
    }
    match token {
        Some(t) if t == config.harness_admin_token => Ok(()),
        _ => bail!("unauthorized"),
    }
}

/// Register a new harness bundle from a manifest.
pub fn handle_register_bundle(journal: &JournalStore, body: &Value) -> Result<Value> {
    let declared_hash = body.get("bundle_hash").and_then(Value::as_str);
    let manifest: HarnessBundleManifest = manifest::validate_manifest(body, declared_hash)?;
    let bundle_hash = manifest::compute_bundle_hash(&manifest);

    // Build prepared operations to verify they are valid.
    let _prepared: Vec<PreparedOperation> = manifest
        .operations
        .iter()
        .map(|op| manifest::prepare_operation(op, &bundle_hash))
        .collect();

    // Persist the canonical manifest (re-serialized from validated struct,
    // not the raw request body — this strips declared_hash, normalizes key
    // order, and ensures consistency).
    let canonical_json = serde_json::to_string(&manifest)?;
    let now = chrono::Utc::now().to_rfc3339();

    // Check for duplicate (same bundle_id + bundle_version).
    if let Some(existing_hash) =
        find_bundle_by_id_version(journal, &manifest.bundle_id, &manifest.bundle_version)?
    {
        if existing_hash != bundle_hash {
            bail!(
                "conflict: bundle_id={} bundle_version={} already exists with different content",
                manifest.bundle_id,
                manifest.bundle_version
            );
        }
        // Idempotent: same hash already registered.
        return Ok(serde_json::json!({
            "ok": true,
            "bundle_hash": bundle_hash,
            "idempotent": true,
        }));
    }

    insert_bundle(journal, &bundle_hash, &manifest, &canonical_json, &now)?;

    let _ = journal.append_event(
        JournalEventKind::HarnessBundleRegistered,
        None,
        None,
        None,
        serde_json::json!({
            "bundle_hash": bundle_hash,
            "bundle_id": manifest.bundle_id,
            "bundle_version": manifest.bundle_version,
            "operation_count": manifest.operations.len(),
        }),
    );

    Ok(serde_json::json!({
        "ok": true,
        "bundle_hash": bundle_hash,
    }))
}

/// Register or update a runtime endpoint.
pub fn handle_register_runtime(
    journal: &JournalStore,
    bundle_hash: &str,
    endpoint: &str,
) -> Result<Value> {
    let reg = registration::register_runtime(journal, bundle_hash, endpoint)?;
    Ok(serde_json::json!({
        "ok": true,
        "registration_id": reg.registration_id,
        "bundle_hash": reg.bundle_hash,
        "endpoint": reg.endpoint,
    }))
}

/// List all bundles.
pub fn handle_list_bundles(journal: &JournalStore) -> Result<Value> {
    let bundles = list_bundles(journal)?;
    Ok(serde_json::json!({ "ok": true, "bundles": bundles }))
}

/// List all runtime registrations.
pub fn handle_list_registrations(journal: &JournalStore) -> Result<Value> {
    let regs = registration::list_registrations(journal)?;
    Ok(serde_json::json!({ "ok": true, "registrations": regs }))
}

/// Compose a candidate snapshot from a base snapshot and bundle hashes.
pub fn handle_compose_snapshot(
    journal: &JournalStore,
    base_snapshot_id: &str,
    bundle_hashes: &[String],
) -> Result<Value> {
    // Load base snapshot.
    let base = journal.load_registry_snapshot(base_snapshot_id)?;
    let base_snapshot_id = base.snapshot_id.clone();

    // Load bundle hashes (sorted for determinism).
    let mut hashes_sorted = bundle_hashes.to_vec();
    hashes_sorted.sort();

    // Collect prepared operations from all bundles.
    let mut all_ops = base.operations.clone();
    let mut seen_names: std::collections::HashSet<String> =
        all_ops.iter().map(|o| o.name.clone()).collect();

    for bh in &hashes_sorted {
        let manifest = load_bundle_manifest(journal, bh)?;
        for op in &manifest.operations {
            if !seen_names.insert(op.name.clone()) {
                bail!("operation name conflict: '{}' in bundle {bh}", op.name);
            }
            let prepared = manifest::prepare_operation(op, bh);
            all_ops.push(prepared.spec);
        }
    }

    // Create the snapshot (idempotent: same specs → same ID).
    let snap = journal.create_registry_snapshot(all_ops)?;

    let _ = journal.append_event(
        JournalEventKind::RegistrySnapshotComposed,
        None,
        None,
        None,
        serde_json::json!({
            "base_snapshot_id": base_snapshot_id,
            "bundle_hashes": hashes_sorted,
            "candidate_snapshot_id": snap.snapshot_id,
            "operation_count": snap.operations.len(),
        }),
    );

    Ok(serde_json::json!({
        "ok": true,
        "snapshot_id": snap.snapshot_id,
    }))
}

/// Activate a snapshot (or rollback to a historical one).
pub fn handle_activate_snapshot(
    journal: &JournalStore,
    snapshot_id: &str,
    correlation_id: Option<&str>,
) -> Result<Value> {
    let previous = journal.current_registry_snapshot_id().ok();
    journal.activate_registry_snapshot(snapshot_id)?;

    let corr = correlation_id.unwrap_or("admin").to_string();
    let _ = journal.append_event(
        JournalEventKind::RegistrySnapshotActivated,
        None,
        None,
        Some(&corr),
        serde_json::json!({
            "previous_snapshot_id": previous,
            "new_snapshot_id": snapshot_id,
        }),
    );

    Ok(serde_json::json!({
        "ok": true,
        "previous_snapshot_id": previous,
        "new_snapshot_id": snapshot_id,
    }))
}

/// Handle grant operations.
pub fn handle_grant_operation(
    journal: &JournalStore,
    channel: &str,
    operation_name: &str,
) -> Result<Value> {
    let grant = grants::grant_operation(journal, channel, operation_name)?;
    Ok(serde_json::json!({
        "ok": true,
        "channel": grant.channel,
        "operation_name": grant.operation_name,
        "action": "granted",
    }))
}

/// Handle revoke operations.
pub fn handle_revoke_operation(
    journal: &JournalStore,
    channel: &str,
    operation_name: &str,
) -> Result<Value> {
    grants::revoke_operation(journal, channel, operation_name)?;
    Ok(serde_json::json!({
        "ok": true,
        "channel": channel,
        "operation_name": operation_name,
        "action": "revoked",
    }))
}

/// List all grants.
pub fn handle_list_grants(journal: &JournalStore, channel: Option<&str>) -> Result<Value> {
    let g = grants::list_grants(journal, channel)?;
    Ok(serde_json::json!({ "ok": true, "grants": g }))
}

/// Get the current registry snapshot info.
pub fn handle_registry_info(journal: &JournalStore) -> Result<Value> {
    let current_id = journal.current_registry_snapshot_id().ok();
    let current = current_id
        .as_ref()
        .and_then(|id| journal.load_registry_snapshot(id).ok())
        .map(|s| {
            serde_json::json!({
                "snapshot_id": s.snapshot_id,
                "operation_count": s.operations.len(),
                "operations": s.operations.iter().map(|op| serde_json::json!({
                    "name": op.name,
                    "risk": format!("{:?}", op.risk),
                    "binding_kind": format!("{:?}", op.binding_kind),
                    "binding_key": op.binding_key,
                })).collect::<Vec<_>>(),
            })
        });

    Ok(serde_json::json!({
        "ok": true,
        "current_snapshot_id": current_id,
        "current_snapshot": current,
    }))
}

// ---- Private helpers ----

fn find_bundle_by_id_version(
    journal: &JournalStore,
    bundle_id: &str,
    bundle_version: &str,
) -> Result<Option<String>> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let result = conn.query_row(
        "SELECT bundle_hash FROM harness_bundles WHERE bundle_id = ?1 AND bundle_version = ?2",
        rusqlite::params![bundle_id, bundle_version],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(h) => Ok(Some(h)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn insert_bundle(
    journal: &JournalStore,
    bundle_hash: &str,
    manifest: &HarnessBundleManifest,
    canonical_json: &str,
    now: &str,
) -> Result<()> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    conn.execute(
        "INSERT INTO harness_bundles (bundle_hash, manifest_version, protocol_version, bundle_id, bundle_version, manifest_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            bundle_hash,
            manifest.manifest_version,
            manifest.protocol_version,
            manifest.bundle_id,
            manifest.bundle_version,
            canonical_json,
            now,
        ],
    )?;
    Ok(())
}

fn list_bundles(journal: &JournalStore) -> Result<Vec<Value>> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let mut stmt = conn.prepare(
        "SELECT bundle_hash, manifest_version, protocol_version, bundle_id, bundle_version, manifest_json, created_at
         FROM harness_bundles ORDER BY created_at"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "bundle_hash": row.get::<_, String>(0)?,
            "manifest_version": row.get::<_, String>(1)?,
            "protocol_version": row.get::<_, String>(2)?,
            "bundle_id": row.get::<_, String>(3)?,
            "bundle_version": row.get::<_, String>(4)?,
            "created_at": row.get::<_, String>(6)?,
        }))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_bundle_manifest(
    journal: &JournalStore,
    bundle_hash: &str,
) -> Result<HarnessBundleManifest> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let json_str: String = conn.query_row(
        "SELECT manifest_json FROM harness_bundles WHERE bundle_hash = ?1",
        rusqlite::params![bundle_hash],
        |row| row.get(0),
    )?;
    let manifest: HarnessBundleManifest = serde_json::from_str(&json_str)?;
    Ok(manifest)
}
