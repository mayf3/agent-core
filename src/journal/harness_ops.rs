//! Harness manifest and registry activation operations on JournalStore.
//! These methods use the same SQLite connection as the rest of the Journal,
//! sharing its mutex. All activation transactions use CAS (compare-and-swap)
//! to prevent concurrent overwrites.

use crate::domain::*;
use crate::harness::control::{ApprovedHarnessChange, RegistryActivationResult};
use crate::harness::manifest::HarnessManifest;
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Transaction};

impl super::JournalStore {
    /// Register a new harness manifest. Idempotent: same content produces the
    /// same manifest_id and returns the existing row. Different content for the
    /// same manifest_id returns an error.
    ///
    /// Validates that manifest.manifest_id matches the computed digest.
    pub fn register_harness_manifest(&self, manifest: &HarnessManifest) -> Result<String> {
        // Run all validations before any database access.
        manifest.validate_all()?;

        // Verify manifest_id matches computed digest.
        let computed_id = manifest.compute_manifest_id()?;
        if manifest.manifest_id != computed_id {
            bail!(
                "manifest_id mismatch: got {}, computed {}",
                manifest.manifest_id,
                computed_id
            );
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        let manifest_id = &manifest.manifest_id;

        // Check if this manifest_id already exists.
        let existing: Option<String> = conn
            .query_row(
                "SELECT canonical_digest FROM harness_manifests WHERE manifest_id = ?1",
                params![manifest_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(existing_digest) = existing {
            // Same manifest_id but different content?
            let content_digest = manifest.compute_manifest_id()?;
            if existing_digest != content_digest {
                bail!("manifest_id {manifest_id} already registered with different content");
            }
            return Ok(manifest_id.clone());
        }

        // Also check for duplicate operation_name.
        let op_exists: Option<String> = conn
            .query_row(
                "SELECT manifest_id FROM harness_manifests WHERE operation_name = ?1",
                params![&manifest.operation_name],
                |row| row.get(0),
            )
            .ok();
        if let Some(existing_mid) = op_exists {
            bail!(
                "operation {} is already registered by manifest {existing_mid}",
                manifest.operation_name
            );
        }

        let content_digest = manifest.compute_manifest_id()?;

        conn.execute(
            "INSERT INTO harness_manifests
             (manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
              operation_name, description, input_schema_json, output_schema_json,
              idempotent, created_at, canonical_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                manifest_id,
                &manifest.harness_id,
                &manifest.artifact_digest,
                &manifest.protocol_version,
                &manifest.endpoint,
                &manifest.operation_name,
                &manifest.description,
                serde_json::to_string(&manifest.input_schema)?,
                serde_json::to_string(&manifest.output_schema)?,
                manifest.idempotent as i64,
                manifest.created_at.to_rfc3339(),
                &content_digest,
            ],
        )?;

        // Record journal event.
        let payload = serde_json::json!({
            "manifest_id": manifest_id,
            "harness_id": manifest.harness_id,
            "artifact_digest": manifest.artifact_digest,
            "operation_name": manifest.operation_name,
            "protocol_version": manifest.protocol_version,
        });
        drop(conn); // release before append_event acquires its own lock
        self.append_event(
            JournalEventKind::HarnessManifestRegistered,
            None,
            None,
            Some(manifest_id),
            payload,
        )?;

        Ok(manifest_id.clone())
    }

    /// Register a harness manifest inside an existing transaction. Only the
    /// INSERT and the HarnessManifestRegistered event happen in the tx;
    /// validation must be performed by the caller before calling this.
    /// Returns the manifest_id.
    pub fn register_harness_manifest_in_tx(
        &self,
        tx: &Transaction<'_>,
        manifest: &HarnessManifest,
    ) -> Result<String> {
        let manifest_id = &manifest.manifest_id;
        let content_digest = manifest.compute_manifest_id()?;
        tx.execute(
            "INSERT OR IGNORE INTO harness_manifests
             (manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
              operation_name, description, input_schema_json, output_schema_json,
              idempotent, created_at, canonical_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                manifest_id,
                &manifest.harness_id,
                &manifest.artifact_digest,
                &manifest.protocol_version,
                &manifest.endpoint,
                &manifest.operation_name,
                &manifest.description,
                serde_json::to_string(&manifest.input_schema)?,
                serde_json::to_string(&manifest.output_schema)?,
                manifest.idempotent as i64,
                manifest.created_at.to_rfc3339(),
                &content_digest,
            ],
        )?;
        // Record HarnessManifestRegistered in the same transaction.
        let payload = serde_json::json!({
            "manifest_id": manifest_id,
            "harness_id": manifest.harness_id,
            "artifact_digest": manifest.artifact_digest,
            "operation_name": manifest.operation_name,
            "protocol_version": manifest.protocol_version,
        });
        super::queue::append_event_tx(
            tx,
            JournalEventKind::HarnessManifestRegistered,
            None,
            None,
            Some(manifest_id),
            payload,
        )?;
        Ok(manifest_id.clone())
    }

    /// Load a harness manifest by ID.
    pub fn load_harness_manifest(&self, manifest_id: &str) -> Result<Option<HarnessManifest>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;

        let row: Option<(
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            bool,
            String,
        )> = conn
            .query_row(
                "SELECT manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
                        operation_name, description, input_schema_json, output_schema_json,
                        idempotent, created_at
                 FROM harness_manifests WHERE manifest_id = ?1",
                params![manifest_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get::<_, i64>(9)? != 0,
                        row.get(10)?,
                    ))
                },
            )
            .ok();

        match row {
            Some((mid, hid, ad, pv, ep, on, desc, is_json, os_json, idemp, ca_str)) => {
                let input_schema: serde_json::Value = serde_json::from_str(&is_json)
                    .map_err(|e| anyhow!("invalid input_schema_json for {mid}: {e}"))?;
                let output_schema: serde_json::Value = serde_json::from_str(&os_json)
                    .map_err(|e| anyhow!("invalid output_schema_json for {mid}: {e}"))?;
                let created_at = chrono::DateTime::parse_from_rfc3339(&ca_str)
                    .map_err(|e| anyhow!("invalid created_at for {mid}: {e}"))?
                    .with_timezone(&chrono::Utc);

                // Verify protocol_version.
                if pv != "external-harness-v1" {
                    return Err(anyhow!("invalid protocol_version for {mid}: {pv:?}"));
                }

                // Verify manifest_id matches recomputed digest.
                let check = HarnessManifest {
                    manifest_id: mid.clone(),
                    harness_id: hid,
                    artifact_digest: ad,
                    protocol_version: pv,
                    endpoint: ep,
                    operation_name: on,
                    description: desc,
                    input_schema: input_schema.clone(),
                    output_schema: output_schema.clone(),
                    idempotent: idemp,
                    created_at,
                };
                let recomputed = check.compute_manifest_id()?;
                if check.manifest_id != recomputed {
                    return Err(anyhow!(
                        "manifest {mid}: stored manifest_id does not match recomputed digest"
                    ));
                }
                Ok(Some(check))
            }
            None => Ok(None),
        }
    }

    /// Enable a harness: create a new snapshot with the external operation,
    /// atomically update the active registry state, and record a journal event.
    ///
    /// Uses CAS on `expected_current_snapshot_id` and `version` to prevent
    /// concurrent activation races.
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
            risk: Risk::ReadOnly,
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

    /// Disable a harness: create a new snapshot WITHOUT the external operation,
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

    /// Atomic activation: BEGIN IMMEDIATE, CAS on expected_snapshot_id,
    /// update registry_state, record journal event, commit, update memory cache.
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

    /// Initialize the registry_state row (singleton) at first boot.
    /// Called during initialize_registry when the baseline snapshot is created.
    pub fn init_registry_state(&self, snapshot_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "INSERT OR IGNORE INTO registry_state (singleton_id, active_snapshot_id, version, updated_at)
             VALUES (1, ?1, 1, ?2)",
            params![snapshot_id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Load the active snapshot ID from the persisted registry_state table.
    /// Used during restart to recover the previous activation state.
    pub fn load_active_snapshot_from_state(&self) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let result: Option<String> = conn
            .query_row(
                "SELECT active_snapshot_id FROM registry_state WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .ok();
        Ok(result)
    }
}
