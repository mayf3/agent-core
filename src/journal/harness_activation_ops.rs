//! Harness activation operations — enable, disable, and atomic
//! registry snapshot activation with CAS + journal event.
//! Extracted from the same impl super::JournalStore block.

use crate::domain::*;
use crate::harness::control::{ApprovedHarnessChange, RegistryActivationResult};

use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::params;

/// Determine risk for an external harness operation by convention.
/// Write operations (containing _write, _mkdir, or _exec) are Risk::Write.
/// All others default to Risk::ReadOnly.
fn risk_for_external_op(operation_name: &str) -> Risk {
    if operation_name.contains("_write")
        || operation_name.contains("_mkdir")
        || operation_name.contains("_exec")
    {
        Risk::Write
    } else {
        Risk::ReadOnly
    }
}

impl super::JournalStore {
    pub fn enable_harness(
        &self,
        approved: &ApprovedHarnessChange,
    ) -> Result<RegistryActivationResult> {
        let manifest_id = &approved.intent.manifest_id;
        let expected_snapshot_id = &approved.intent.expected_snapshot_id;

        // Load the manifest.
        let manifest = self
            .load_harness_manifest(manifest_id)?
            .ok_or_else(|| anyhow!("manifest not found: {manifest_id}"))?;

        // Load the current snapshot.
        let current = self.current_registry_snapshot_id()?;
        if &current != expected_snapshot_id {
            bail!("snapshot_conflict: expected {expected_snapshot_id}, current {current}");
        }

        let current_snap = self.load_registry_snapshot(&current)?;

        // Check if the operation is already in the current snapshot.
        if current_snap.lookup(&manifest.operation_name).is_some() {
            // Operation already present — idempotent, return current.
            return Ok(RegistryActivationResult {
                previous_snapshot_id: current.clone(),
                active_snapshot_id: current,
                changed: false,
            });
        }

        // Build new spec list: existing ops + new external op.
        let mut new_specs: Vec<OperationSpec> = current_snap.operations.clone();
        new_specs.push(OperationSpec {
            name: manifest.operation_name.clone(),
            risk: risk_for_external_op(&manifest.operation_name),
            description: manifest.description.clone(),
            parameters: manifest.input_schema.clone(),
            idempotent: manifest.idempotent,
            binding_kind: BindingKind::External,
            binding_key: manifest_id.clone(),
        });

        // Compute new snapshot ID.
        let new_snapshot = self.create_registry_snapshot(new_specs)?;
        let new_snapshot_id = new_snapshot.snapshot_id.clone();

        // Atomically: update registry_state and record journal event.
        self.activate_registry_snapshot_atomic(
            &new_snapshot_id,
            expected_snapshot_id,
            &approved.decision_id,
            "enable",
            manifest_id,
            &manifest.operation_name,
        )?;

        Ok(RegistryActivationResult {
            previous_snapshot_id: current,
            active_snapshot_id: new_snapshot_id,
            changed: true,
        })
    }

    /// (same as enable)
    /// atomically update the active registry state, and record a journal event.
    pub fn disable_harness(
        &self,
        approved: &ApprovedHarnessChange,
    ) -> Result<RegistryActivationResult> {
        let manifest_id = &approved.intent.manifest_id;
        let expected_snapshot_id = &approved.intent.expected_snapshot_id;

        // Load the manifest to get the operation_name.
        let manifest = self
            .load_harness_manifest(manifest_id)?
            .ok_or_else(|| anyhow!("manifest not found: {manifest_id}"))?;

        // Load the current snapshot.
        let current = self.current_registry_snapshot_id()?;
        if &current != expected_snapshot_id {
            bail!("snapshot_conflict: expected {expected_snapshot_id}, current {current}");
        }

        let current_snap = self.load_registry_snapshot(&current)?;

        // Check if the operation is already absent.
        if current_snap.lookup(&manifest.operation_name).is_none() {
            // Already absent — idempotent.
            return Ok(RegistryActivationResult {
                previous_snapshot_id: current.clone(),
                active_snapshot_id: current,
                changed: false,
            });
        }

        // Build new spec list: existing ops minus the external one.
        let new_specs: Vec<OperationSpec> = current_snap
            .operations
            .iter()
            .filter(|op| op.name != manifest.operation_name)
            .cloned()
            .collect();

        let new_snapshot = self.create_registry_snapshot(new_specs)?;
        let new_snapshot_id = new_snapshot.snapshot_id.clone();

        // Atomically: update registry_state and record journal event.
        self.activate_registry_snapshot_atomic(
            &new_snapshot_id,
            expected_snapshot_id,
            &approved.decision_id,
            "disable",
            manifest_id,
            &manifest.operation_name,
        )?;

        Ok(RegistryActivationResult {
            previous_snapshot_id: current,
            active_snapshot_id: new_snapshot_id,
            changed: true,
        })
    }

    /// Atomic activation: BEGIN IMMEDIATE, CAS, update registry, journal event, commit.
    fn activate_registry_snapshot_atomic(
        &self,
        new_snapshot_id: &str,
        expected_snapshot_id: &str,
        decision_id: &str,
        action: &str,
        manifest_id: &str,
        operation_name: &str,
    ) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| anyhow!("cannot begin transaction"))?;

        // CAS: read current registry_state and verify.
        let (db_snapshot_id, db_version): (String, i64) = tx
            .query_row(
                "SELECT active_snapshot_id, version FROM registry_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow!("registry_state not initialized"))?;

        if db_snapshot_id != expected_snapshot_id {
            bail!("snapshot_conflict: registry_state has {db_snapshot_id}, expected {expected_snapshot_id}");
        }

        let new_version = db_version + 1;

        // Update registry_state with CAS (version check).
        let changed = tx.execute(
            "UPDATE registry_state SET active_snapshot_id = ?1, version = ?2, updated_at = ?3
             WHERE singleton_id = 1 AND version = ?4",
            params![
                new_snapshot_id,
                new_version,
                Utc::now().to_rfc3339(),
                db_version,
            ],
        )?;
        if changed == 0 {
            bail!("snapshot_conflict: version CAS failed");
        }

        // Record journal event.
        let payload = serde_json::json!({
            "action": action,
            "manifest_id": manifest_id,
            "operation_name": operation_name,
            "previous_snapshot_id": expected_snapshot_id,
            "new_snapshot_id": new_snapshot_id,
            "decision_id": decision_id,
        });
        let kind_text = "RegistrySnapshotActivated";
        let event_id = EventId::new();
        let created_at = Utc::now();
        let payload_json = serde_json::to_string(&payload)?;

        // Get previous hash for chain.
        let previous: Option<(i64, String)> = tx
            .query_row(
                "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        let sequence = previous.as_ref().map(|(seq, _)| seq + 1).unwrap_or(1);
        let previous_hash = previous.map(|(_, hash)| hash);
        let hash = crate::journal::hash_chain::event_hash(
            previous_hash.as_deref(),
            sequence,
            kind_text,
            &payload_json,
        );

        tx.execute(
            "INSERT INTO journal_events
             (sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                sequence,
                event_id.0,
                Option::<String>::None,
                Option::<String>::None,
                decision_id,
                kind_text,
                payload_json,
                previous_hash,
                hash,
                created_at.to_rfc3339(),
            ],
        )?;

        tx.commit()?;

        // Release DB lock before updating memory cache.
        drop(conn);

        // Update memory cache AFTER successful commit.
        *self.current_snapshot_id.lock().unwrap() = Some(new_snapshot_id.to_string());

        Ok(())
    }
}
