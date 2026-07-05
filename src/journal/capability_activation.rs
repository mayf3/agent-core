//! Atomic activation of approved capability proposals. All steps (Manifest
//! registration (when provided), Registry Snapshot composition, CAS state
//! update, proposal status, Journal events) execute in a single SQLite
//! transaction.

use crate::domain::capability_change::*;
use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use crate::registry::snapshot::{compute_snapshot_id, OperationSpec as SnapSpec, RegistrySnapshot};
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Transaction, TransactionBehavior};
use serde_json::json;

impl super::JournalStore {
    /// Atomically activate a proposal, optionally registering a harness
    /// manifest in the same transaction.
    ///
    /// When `manifest` is `Some`, the manifest row + HarnessManifestRegistered
    /// event are inserted before the activation steps. All operations (manifest
    /// registration, Registry Snapshot, CAS state update, proposal status,
    /// journal events) happen inside a single BEGIN IMMEDIATE transaction.
    ///
    /// On success the in-memory registry cache is refreshed. On failure
    /// (manifest registration fails, proposal not Pending, expired at tx time,
    /// target agent mismatch, stale snapshot, CAS conflict, event write failure)
    /// the ENTIRE transaction rolls back: no manifest row persists, no
    /// HarnessManifestRegistered event, no Registry Snapshot, no status change,
    /// no terminal events.
    pub fn activate_proposal_atomic(
        &self,
        proposal: &CapabilityChangeProposal,
        principal: &str,
        new_operations: Vec<SnapSpec>,
        expected_snapshot_id: &str,
        decision_id: &str,
        manifest: Option<&HarnessManifest>,
        expected_agent_id: &AgentId,
    ) -> Result<String> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // 0. Register the harness manifest inside the transaction (if provided).
        if let Some(m) = manifest {
            self.register_harness_manifest_in_tx(&tx, m)
                .map_err(|e| anyhow!("manifest_registration_failed:{e}"))?;
        }

        // 1. Verify proposal is still PendingApproval and re-read authoritative
        //    fields from the database (defeats TOCTOU from pre-tx reads).
        let expiry_and_status: (String, String, String) = tx
            .query_row(
                "SELECT status, expires_at, target_agent_id FROM capability_change_proposals WHERE proposal_id = ?1",
                params![proposal.proposal_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow!("proposal_not_found"))?;
        let (cur_status, db_expires_at, db_target_agent_id) = expiry_and_status;
        if cur_status != "PendingApproval" {
            bail!("proposal_not_pending: {cur_status}");
        }

        // 1a. Check expiry with the transaction's fresh timestamp.
        let decision_now = Utc::now();
        let persisted_expiry = chrono::DateTime::parse_from_rfc3339(&db_expires_at)
            .map_err(|_| anyhow!("invalid_persisted_expires_at"))?
            .with_timezone(&chrono::Utc);
        if decision_now >= persisted_expiry {
            bail!("proposal_expired: stale_expiry_at_tx_time");
        }

        // 1b. Verify target_agent_id matches the expected configured agent.
        if db_target_agent_id != expected_agent_id.0 {
            bail!(
                "target_agent_mismatch: has {db_target_agent_id} expected {}",
                expected_agent_id.0
            );
        }

        // 2. Verify active snapshot hasn't changed.
        let (db_snap, db_ver): (String, i64) = tx
            .query_row(
                "SELECT active_snapshot_id, version FROM registry_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow!("registry_state_not_found"))?;
        if db_snap != expected_snapshot_id {
            bail!("stale_expected_snapshot: has {db_snap} expected {expected_snapshot_id}");
        }

        // 3. Create the new RegistrySnapshot.
        let snapshot_id = compute_snapshot_id(&new_operations)?;
        let created_at = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO registry_snapshots (snapshot_id, created_at, operation_count, canonical_digest)
             VALUES (?1, ?2, ?3, ?4)",
            params![&snapshot_id, &created_at, new_operations.len() as i64, &snapshot_id],
        )?;
        let mut sorted = new_operations.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for op in &sorted {
            tx.execute(
                "INSERT INTO registry_snapshot_operations
                 (snapshot_id, operation_name, risk, description, parameters_json, idempotent, binding_kind, binding_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![&snapshot_id, &op.name, format!("{:?}", op.risk),
                    &op.description, serde_json::to_string(&op.parameters)?,
                    op.idempotent as i64, format!("{:?}", op.binding_kind), &op.binding_key],
            )?;
        }

        // 4. CAS update registry_state.
        let new_version = db_ver + 1;
        let changed = tx.execute(
            "UPDATE registry_state SET active_snapshot_id = ?1, version = ?2, updated_at = ?3
             WHERE singleton_id = 1 AND version = ?4",
            params![&snapshot_id, new_version, Utc::now().to_rfc3339(), db_ver],
        )?;
        if changed == 0 {
            bail!("registry_activation_conflict");
        }

        // 5. Update proposal to Activated.
        tx.execute(
            "UPDATE capability_change_proposals SET status = 'Activated',
             decided_at = ?1, decided_by = ?2, decision_reason = ?3,
             activated_snapshot_id = ?4
             WHERE proposal_id = ?5",
            params![
                Utc::now().to_rfc3339(),
                principal,
                "activated",
                &snapshot_id,
                proposal.proposal_id
            ],
        )?;

        // 6. Write RegistrySnapshotActivated.
        let snap_payload = json!({
            "action": "capability_activation", "previous_snapshot_id": expected_snapshot_id,
            "new_snapshot_id": snapshot_id, "decision_id": decision_id,
        });
        append_journal_tx(
            &tx,
            "RegistrySnapshotActivated",
            &proposal.origin_run_id,
            &proposal.origin_session_id,
            decision_id,
            &snap_payload,
        )?;

        // 7. Write CapabilityChangeActivated.
        let cap_payload = json!({
            "proposal_id": proposal.proposal_id, "decided_by": principal,
            "previous_snapshot_id": expected_snapshot_id, "new_snapshot_id": snapshot_id,
        });
        append_journal_tx(
            &tx,
            "CapabilityChangeActivated",
            &proposal.origin_run_id,
            &proposal.origin_session_id,
            &proposal.proposal_id,
            &cap_payload,
        )?;

        tx.commit()?;
        drop(conn);

        // 8. Update in-memory registry cache.
        *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.clone());

        Ok(snapshot_id)
    }

    /// Atomically activate a schema-only upgrade for an existing External
    /// operation.  Compared to `activate_proposal_atomic`, this function:
    ///
    /// - Does NOT require `current_snapshot_id == expected_snapshot_id`.
    /// - Instead loads the expected snapshot, looks up the target operation,
    ///   and verifies it has NOT changed in the current active snapshot
    ///   (per-operation CAS).
    /// - Only permits changes to `manifest_id`, `description`, `input_schema`,
    ///   and `output_schema`.  Artifact, endpoint, harness, protocol, and
    ///   idempotent must remain identical.
    /// - Returns the new snapshot ID on success.
    pub fn activate_schema_upgrade_atomic(
        &self,
        proposal: &CapabilityChangeProposal,
        principal: &str,
        new_operations: Vec<SnapSpec>,
        _expected_snapshot_id: &str,
        decision_id: &str,
        manifest: Option<&HarnessManifest>,
        expected_agent_id: &AgentId,
        expected_snapshot: &RegistrySnapshot,
    ) -> Result<String> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // 0. Register the harness manifest inside the transaction (if provided).
        //    For schema upgrades, first remove the old manifest row (by
        //    operation_name) so the UNIQUE(operation_name) constraint does not
        //    block the new manifest registration.
        if let Some(m) = manifest {
            tx.execute(
                "DELETE FROM harness_manifests WHERE operation_name = ?1",
                params![m.operation_name],
            )?;
            self.register_harness_manifest_replace_tx(&tx, m)
                .map_err(|e| anyhow!("manifest_registration_failed:{e}"))?;
        }

        // 1. Verify proposal is still PendingApproval and re-read authoritative
        //    fields from the database.
        let expiry_and_status: (String, String, String) = tx
            .query_row(
                "SELECT status, expires_at, target_agent_id FROM capability_change_proposals WHERE proposal_id = ?1",
                params![proposal.proposal_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow!("proposal_not_found"))?;
        let (cur_status, db_expires_at, db_target_agent_id) = expiry_and_status;
        if cur_status != "PendingApproval" {
            bail!("proposal_not_pending: {cur_status}");
        }

        // 1a. Check expiry with the transaction's fresh timestamp.
        let decision_now = Utc::now();
        let persisted_expiry = chrono::DateTime::parse_from_rfc3339(&db_expires_at)
            .map_err(|_| anyhow!("invalid_persisted_expires_at"))?
            .with_timezone(&chrono::Utc);
        if decision_now >= persisted_expiry {
            bail!("proposal_expired: stale_expiry_at_tx_time");
        }

        // 1b. Verify target_agent_id matches the expected configured agent.
        if db_target_agent_id != expected_agent_id.0 {
            bail!(
                "target_agent_mismatch: has {db_target_agent_id} expected {}",
                expected_agent_id.0
            );
        }

        // 2. Get current registry state.
        let (db_snap, db_ver): (String, i64) = tx
            .query_row(
                "SELECT active_snapshot_id, version FROM registry_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow!("registry_state_not_found"))?;

        // 3. Load the current snapshot within the transaction and verify the
        //    TARGET operation has NOT changed since the expected snapshot.
        let current_snap = Self::load_snapshot_from_conn(&tx, &db_snap)?;
        for op_spec in &new_operations {
            let old_spec = expected_snapshot
                .lookup(&op_spec.name)
                .ok_or_else(|| anyhow!("expected_op_not_found:{}", op_spec.name))?;
            let current_spec = current_snap
                .lookup(&op_spec.name)
                .ok_or_else(|| anyhow!("current_op_not_found:{}", op_spec.name))?;
            if old_spec != current_spec {
                bail!("target_operation_changed:{}", op_spec.name);
            }
        }
        for op_spec in &new_operations {
            let old_spec = expected_snapshot
                .lookup(&op_spec.name)
                .ok_or_else(|| anyhow!("expected_op_not_found:{}", op_spec.name))?;
            let current_spec = current_snap
                .lookup(&op_spec.name)
                .ok_or_else(|| anyhow!("current_op_not_found:{}", op_spec.name))?;
            // The operation must not have been modified since the expected snapshot.
            if old_spec != current_spec {
                bail!("target_operation_changed:{}", op_spec.name);
            }
        }

        // 4. Create the new RegistrySnapshot from the provided operations.
        let snapshot_id = compute_snapshot_id(&new_operations)?;
        let created_at = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO registry_snapshots (snapshot_id, created_at, operation_count, canonical_digest)
             VALUES (?1, ?2, ?3, ?4)",
            params![&snapshot_id, &created_at, new_operations.len() as i64, &snapshot_id],
        )?;
        let mut sorted = new_operations.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for op in &sorted {
            tx.execute(
                "INSERT INTO registry_snapshot_operations
                 (snapshot_id, operation_name, risk, description, parameters_json, idempotent, binding_kind, binding_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![&snapshot_id, &op.name, format!("{:?}", op.risk),
                    &op.description, serde_json::to_string(&op.parameters)?,
                    op.idempotent as i64, format!("{:?}", op.binding_kind), &op.binding_key],
            )?;
        }

        // 5. CAS update registry_state.
        let new_version = db_ver + 1;
        let changed = tx.execute(
            "UPDATE registry_state SET active_snapshot_id = ?1, version = ?2, updated_at = ?3
             WHERE singleton_id = 1 AND version = ?4",
            params![&snapshot_id, new_version, Utc::now().to_rfc3339(), db_ver],
        )?;
        if changed == 0 {
            bail!("registry_activation_conflict");
        }

        // 6. Update proposal to Activated.
        tx.execute(
            "UPDATE capability_change_proposals SET status = 'Activated',
             decided_at = ?1, decided_by = ?2, decision_reason = ?3,
             activated_snapshot_id = ?4
             WHERE proposal_id = ?5",
            params![
                Utc::now().to_rfc3339(),
                principal,
                "activated",
                &snapshot_id,
                proposal.proposal_id
            ],
        )?;

        // 7. Write RegistrySnapshotActivated with schema_upgrade action.
        let snap_payload = json!({
            "action": "schema_upgrade", "previous_snapshot_id": &db_snap,
            "new_snapshot_id": &snapshot_id, "decision_id": decision_id,
        });
        append_journal_tx(
            &tx,
            "RegistrySnapshotActivated",
            &proposal.origin_run_id,
            &proposal.origin_session_id,
            decision_id,
            &snap_payload,
        )?;

        // 8. Write CapabilityChangeActivated.
        let cap_payload = json!({
            "proposal_id": proposal.proposal_id, "decided_by": principal,
            "previous_snapshot_id": &db_snap, "new_snapshot_id": &snapshot_id,
        });
        append_journal_tx(
            &tx,
            "CapabilityChangeActivated",
            &proposal.origin_run_id,
            &proposal.origin_session_id,
            &proposal.proposal_id,
            &cap_payload,
        )?;

        tx.commit()?;
        drop(conn);

        // 9. Update in-memory registry cache.
        *self.current_snapshot_id.lock().unwrap() = Some(snapshot_id.clone());

        Ok(snapshot_id)
    }
}

fn append_journal_tx(
    tx: &Transaction<'_>,
    kind: &str,
    run_id: &RunId,
    session_id: &SessionId,
    correlation_id: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let event_id = EventId::new();
    let ts = Utc::now().to_rfc3339();
    let payload_json = serde_json::to_string(payload)?;
    let previous: Option<(i64, String)> = tx
        .query_row(
            "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let seq = previous.as_ref().map(|(s, _)| s + 1).unwrap_or(1);
    let hash = crate::journal::hash_chain::event_hash(
        previous.as_ref().map(|(_, h)| h.as_str()),
        seq,
        kind,
        &payload_json,
    );
    tx.execute(
        "INSERT INTO journal_events (sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![seq, event_id.0, run_id.0, session_id.0, correlation_id, kind, payload_json,
            previous.as_ref().map(|(_, h)| h.as_str()), hash, ts],
    )?;
    Ok(())
}
