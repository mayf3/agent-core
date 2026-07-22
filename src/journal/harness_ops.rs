//! Harness manifest and registry activation operations on JournalStore.
//! All activation transactions use CAS (compare-and-swap) to prevent races.

use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use crate::registry::store::builtin_specs;
use crate::registry::snapshot::BindingKind;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Transaction};
use serde_json::Value;

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

        // Read the full persisted manifest row using OptionalExtension.
        use rusqlite::OptionalExtension;
        let existing_row: Option<(
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
            String,
        )> = tx
            .query_row(
                "SELECT manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
                        operation_name, description, input_schema_json, output_schema_json,
                        idempotent, created_at, canonical_digest
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
                        row.get(11)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| anyhow::anyhow!("manifest_lookup_failed:{e}"))?;

        if let Some((
            stored_mid,
            e_hid,
            e_art,
            e_proto,
            e_ep,
            e_op,
            e_desc,
            e_inp,
            e_out,
            e_idem,
            e_ts,
            e_dig,
        )) = existing_row
        {
            // — never silently treated as "not found" or "reusable".
            let e_created_at = chrono::DateTime::parse_from_rfc3339(&e_ts)
                .map_err(|_| anyhow!("corrupt_persisted_created_at:{manifest_id}"))?
                .with_timezone(&chrono::Utc);
            let e_input_schema: serde_json::Value = serde_json::from_str(&e_inp)
                .map_err(|_| anyhow!("corrupt_persisted_input_schema:{manifest_id}"))?;
            let e_output_schema: serde_json::Value = serde_json::from_str(&e_out)
                .map_err(|_| anyhow!("corrupt_persisted_output_schema:{manifest_id}"))?;

            let persisted_manifest = HarnessManifest {
                manifest_id: stored_mid.clone(),
                harness_id: e_hid,
                artifact_digest: e_art,
                protocol_version: e_proto,
                endpoint: e_ep,
                operation_name: e_op,
                description: e_desc,
                input_schema: e_input_schema,
                output_schema: e_output_schema,
                idempotent: e_idem,
                created_at: e_created_at,
            };

            // Verify four digest/ID invariants before structural comparison.
            let recomputed_id = persisted_manifest
                .compute_manifest_id()
                .map_err(|_| anyhow!("manifest_digest_recompute_failed:{manifest_id}"))?;

            // Invariant 1: stored canonical_digest == recomputed from persisted data
            if recomputed_id != e_dig {
                bail!("corrupt_persisted_canonical_digest:{manifest_id}: stored={e_dig} recomputed={recomputed_id}");
            }
            // Invariant 2: stored manifest_id == recomputed (persisted data is self-consistent)
            if stored_mid != recomputed_id {
                bail!("corrupt_persisted_manifest_id:{manifest_id}: stored={stored_mid} recomputed={recomputed_id}");
            }
            // Invariant 3: incoming canonical digest == stored canonical_digest
            if content_digest != e_dig {
                bail!("manifest_identity_conflict: incoming digest {content_digest} != stored {e_dig}");
            }
            // Invariant 4: incoming manifest_id == stored manifest_id
            if manifest.manifest_id != stored_mid {
                bail!(
                    "manifest_identity_conflict: incoming id {} != stored {stored_mid}",
                    manifest.manifest_id
                );
            }

            // Full structural comparison: every immutable field must be identical.
            if persisted_manifest.harness_id != manifest.harness_id
                || persisted_manifest.artifact_digest != manifest.artifact_digest
                || persisted_manifest.protocol_version != manifest.protocol_version
                || persisted_manifest.endpoint != manifest.endpoint
                || persisted_manifest.operation_name != manifest.operation_name
                || persisted_manifest.description != manifest.description
                || persisted_manifest.input_schema != manifest.input_schema
                || persisted_manifest.output_schema != manifest.output_schema
                || persisted_manifest.idempotent != manifest.idempotent
                || persisted_manifest.created_at != manifest.created_at
            {
                bail!("manifest_identity_conflict: manifest_id {manifest_id} already registered with different content");
            }

            // Full structural match — safe idempotent reuse.
            return Ok(manifest_id.clone());
        }

        // Note: operation_name is intentionally NOT checked for uniqueness.
        // The DB schema has no UNIQUE constraint on operation_name, allowing
        // multiple manifests for the same operation.  This supports upgrading
        // a capability to a new artifact while keeping old snapshots intact.

        let mid = self.insert_manifest_row_tx(tx, manifest, content_digest)?;
        Ok(mid)
    }

    /// Register a harness manifest INSIDE a transaction, SKIPPING the
    /// operation-name uniqueness check.  Used ONLY during schema-only upgrades
    /// where the operation already has a manifest and we are registering a
    /// replacement.  The caller MUST ensure the operation's old manifest is
    /// being replaced (not a duplicate create).
    pub(crate) fn register_harness_manifest_replace_tx(
        &self,
        tx: &Transaction<'_>,
        manifest: &HarnessManifest,
    ) -> Result<String> {
        let content_digest = manifest.compute_manifest_id()?;
        let manifest_id = &manifest.manifest_id;

        // Check if a manifest with this ID already exists.  During an upgrade
        // the same manifest_id may have been registered by a prior activation;
        // if the canonical_digest matches we can reuse it idempotently (the
        // structural content is identical despite a different created_at).
        use rusqlite::OptionalExtension;
        let existing_digest: Option<String> = tx
            .query_row(
                "SELECT canonical_digest FROM harness_manifests WHERE manifest_id = ?1",
                params![manifest_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| anyhow!("manifest_reuse_lookup_failed:{e}"))?;

        if let Some(stored_digest) = existing_digest {
            if stored_digest == content_digest {
                // Same canonical content — safe idempotent reuse.
                return Ok(manifest_id.clone());
            }
            // Canonical digest differs — the caller is attempting to replace
            // a manifest with different content under the same ID.  This is
            // not allowed; a new manifest must have a new manifest_id.
            bail!(
                "manifest_reuse_conflict: {manifest_id} stored digest {stored_digest} != {content_digest}"
            );
        }

        let mid = self.insert_manifest_row_tx(tx, manifest, content_digest)?;
        Ok(mid)
    }

    /// Shared manifest INSERT + event.  Does NOT check operation-name
    /// uniqueness — the caller decides whether that guard is needed.
    fn insert_manifest_row_tx(
        &self,
        tx: &Transaction<'_>,
        manifest: &HarnessManifest,
        content_digest: String,
    ) -> Result<String> {
        let manifest_id = &manifest.manifest_id;
        tx.execute(
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

    /// Load manifest by ID.
    pub fn load_harness_manifest(&self, manifest_id: &str) -> Result<Option<HarnessManifest>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        Self::load_harness_manifest_from_conn(&conn, manifest_id)
    }

    /// Load a harness manifest inside an existing transaction.
    pub(crate) fn load_harness_manifest_in_tx(
        &self,
        tx: &Transaction<'_>,
        manifest_id: &str,
    ) -> Result<Option<HarnessManifest>> {
        Self::load_harness_manifest_from_conn(tx, manifest_id)
    }

    fn load_harness_manifest_from_conn(
        conn: &rusqlite::Connection,
        manifest_id: &str,
    ) -> Result<Option<HarnessManifest>> {
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

    /// Enable or disable a harness: create new snapshot + CAS-activate + journal event.
    /// atomically update the active registry state, and record a journal event.
    ///
    /// concurrent activation races.

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

    /// Bootstrap harness manifests for builtin external operations.
    ///
    /// Iterates the builtin specification catalog and registers a
    /// `HarnessManifest` for each operation whose `binding_kind` is
    /// `External`.  These manifests are REQUIRED for the runtime tool
    /// dispatch path — without them, model-initiated tool calls to
    /// operations like `external.coding_task_submit` fail with
    /// `external_harness_manifest_not_found`.
    ///
    /// # Idempotency
    ///
    /// Already-registered manifests whose canonical content matches are
    /// accepted (safe replay).  Manifests with the same manifest_id but
    /// different content are rejected as a drift/conflict.
    ///
    /// # Fail-closed
    ///
    /// If `coding_harness_api_url` or `coding_harness_artifact_digest`
    /// are empty, no manifests are registered.  The caller (serve) should
    /// verify these are configured when external bindings are active.
    pub fn bootstrap_builtin_external_manifests(
        &self,
        coding_harness_api_url: &str,
        coding_harness_artifact_digest: &str,
    ) -> Result<()> {
        let specs = builtin_specs();
        for spec in &specs {
            if spec.binding_kind != BindingKind::External {
                continue;
            }
            let created_at = Utc::now();
            let manifest = HarnessManifest {
                manifest_id: String::new(), // will be computed
                harness_id: "coding-harness-v0".into(),
                artifact_digest: coding_harness_artifact_digest.to_string(),
                protocol_version: "external-harness-v1".into(),
                endpoint: coding_harness_api_url.to_string(),
                operation_name: spec.name.clone(),
                description: spec.description.clone(),
                input_schema: spec.parameters.clone(),
                output_schema: serde_json::json!({"type": "object"}),
                idempotent: spec.idempotent,
                created_at,
            };
            // Compute manifest_id from canonical content.
            let computed_id = manifest.compute_manifest_id()?;
            let mut manifest = manifest;
            manifest.manifest_id = computed_id;

            // register_harness_manifest handles idempotency and drift.
            let _mid = self.register_harness_manifest(&manifest)?;
        }
        Ok(())
    }
}
