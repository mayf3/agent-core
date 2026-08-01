//! Shared Registry Snapshot composition and compare-and-swap activation.

use super::trusted_capability_activation::TrustedDecisionIdentity;
use crate::capabilities::store::Sha256Digest;
use crate::domain::{JournalEventKind, RunId, SessionId};
use crate::harness::manifest::HarnessManifest;
use crate::registry::snapshot::{
    compute_snapshot_id_with_hook_bindings, BindingKind, HookBinding, OperationSpec, Risk,
};
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::json;
use std::collections::HashSet;

pub(crate) struct RegistryActivation {
    pub previous_snapshot_id: String,
    pub new_snapshot_id: String,
}

/// Load the generic hook binding set persisted on a snapshot. Accepts a
/// `Connection` or a `Transaction` (which derefs to `Connection`).
pub(crate) fn load_active_hook_bindings(
    conn: &rusqlite::Connection,
    snapshot_id: &str,
) -> Result<Vec<HookBinding>> {
    let mut stmt = conn.prepare(
        "SELECT contract, hook_id, hook_version, binding_kind, binding_key, provider_id, endpoint
         FROM registry_snapshot_hook_bindings
         WHERE snapshot_id = ?1
         ORDER BY contract",
    )?;
    let rows = stmt.query_map(params![snapshot_id], |row| {
        let contract: String = row.get(0)?;
        let hook_id: String = row.get(1)?;
        let hook_version: String = row.get(2)?;
        let kind_str: String = row.get(3)?;
        let binding_key: String = row.get(4)?;
        let provider_id: String = row.get(5)?;
        let endpoint: String = row.get(6)?;
        let binding_kind = match kind_str.as_str() {
            "Builtin" => BindingKind::Builtin,
            "External" => BindingKind::External,
            _ => BindingKind::Builtin,
        };
        Ok(HookBinding {
            contract,
            hook_id,
            hook_version,
            binding_kind,
            binding_key,
            provider_id,
            endpoint,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}

pub(crate) struct Binding {
    pub governance_approval: bool,
    pub receipt_binding: bool,
    pub proposal_id: String,
    pub proposal_status: String,
    pub submitter: String,
    pub target_agent: String,
    pub origin_session: String,
    pub origin_run: String,
    pub proposal_artifact: String,
    pub proposal_artifact_ref: String,
    pub proposal_evidence: String,
    pub proposal_manifest: String,
    pub proposal_snapshot: String,
    pub requested_operations: String,
    pub proposal_expires_at: String,
    pub link_operation: String,
    pub link_candidate: String,
    pub link_artifact: String,
    pub link_snapshot: String,
    pub link_run: String,
    pub link_hcr: String,
    pub link_claim: String,
    pub link_candidate_id: String,
    pub link_artifact_ref: String,
    pub link_evidence: String,
    pub link_settlement: String,
    pub link_request_digest: String,
    pub link_receipt_digest: String,
    pub link_issuer: String,
    pub link_outcome: String,
    pub link_contract_version: String,
    pub link_profile_id: String,
    pub link_profile_version: String,
    pub link_acceptance_invocation: String,
    pub owner: String,
    pub approval_snapshot: String,
    pub approval_candidate: String,
    pub approval_artifact: String,
    pub approval_manifest: String,
    pub nonce: String,
    pub approval_status: String,
    pub decision_id: Option<String>,
    pub payload_digest: Option<String>,
    pub result_json: Option<String>,
    pub decided_by: Option<String>,
    pub activated_snapshot: Option<String>,
    pub deployment_id: Option<String>,
    pub activation_error: Option<String>,
    pub approval_expires_at: String,
}

/// Insert one immutable Registry Snapshot and make it active with the
/// Registry's version CAS.  Every proposal activation path calls this core;
/// callers remain responsible for their proposal/decision rows and events.
///
/// The new snapshot inherits the complete hook binding set of the currently
/// active snapshot, falling back to the bootstrap binding set when the active
/// snapshot has none. Activation never silently drops or switches hook
/// identity.
pub(crate) fn activate_registry_tx(
    tx: &Transaction<'_>,
    new_operations: &[OperationSpec],
    expected_snapshot_id: &str,
) -> Result<RegistryActivation> {
    let mut names = HashSet::with_capacity(new_operations.len());
    if new_operations.iter().any(|op| !names.insert(&op.name)) {
        bail!("registry_duplicate_operation");
    }

    let (active_snapshot_id, version): (String, i64) = tx
        .query_row(
            "SELECT active_snapshot_id, version FROM registry_state WHERE singleton_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| anyhow::anyhow!("registry_state_not_found"))?;
    if active_snapshot_id != expected_snapshot_id {
        bail!("stale_expected_snapshot: has {active_snapshot_id} expected {expected_snapshot_id}");
    }

    // Inherit the complete hook binding set from the active snapshot; fall
    // back to the bootstrap set so activation never silently drops or
    // switches hook identity.
    let hook_bindings = load_active_hook_bindings(tx, &active_snapshot_id)?;
    let hook_bindings = if hook_bindings.is_empty() {
        crate::registry::store::builtin_hook_bindings()
    } else {
        hook_bindings
    };

    let snapshot_id = compute_snapshot_id_with_hook_bindings(new_operations, &hook_bindings)?;
    let created_at = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO registry_snapshots
         (snapshot_id,created_at,operation_count,canonical_digest)
         VALUES (?1,?2,?3,?4)",
        params![
            snapshot_id,
            created_at,
            new_operations.len() as i64,
            snapshot_id
        ],
    )?;
    let mut sorted = new_operations.to_vec();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for op in &sorted {
        tx.execute(
            "INSERT INTO registry_snapshot_operations
             (snapshot_id,operation_name,risk,description,parameters_json,
              idempotent,binding_kind,binding_key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                snapshot_id,
                op.name,
                format!("{:?}", op.risk),
                op.description,
                serde_json::to_string(&op.parameters)?,
                op.idempotent as i64,
                format!("{:?}", op.binding_kind),
                op.binding_key,
            ],
        )?;
    }
    for b in &hook_bindings {
        tx.execute(
            "INSERT INTO registry_snapshot_hook_bindings
             (snapshot_id,contract,hook_id,hook_version,binding_kind,binding_key,provider_id,endpoint)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                snapshot_id,
                b.contract,
                b.hook_id,
                b.hook_version,
                format!("{:?}", b.binding_kind),
                b.binding_key,
                b.provider_id,
                b.endpoint,
            ],
        )?;
    }

    let changed = tx.execute(
        "UPDATE registry_state
         SET active_snapshot_id=?1,version=?2,updated_at=?3
         WHERE singleton_id=1 AND version=?4",
        params![snapshot_id, version + 1, Utc::now().to_rfc3339(), version],
    )?;
    if changed != 1 {
        bail!("registry_activation_conflict");
    }
    Ok(RegistryActivation {
        previous_snapshot_id: active_snapshot_id,
        new_snapshot_id: snapshot_id,
    })
}

pub(crate) fn validate_capability_manifest(
    manifest: &HarnessManifest,
    identity: &TrustedDecisionIdentity,
) -> Result<()> {
    manifest.validate_all()?;
    if manifest.compute_manifest_id()? != manifest.manifest_id
        || manifest.harness_id != "capability-host-v0"
        || manifest.endpoint != "http://127.0.0.1:7300/execute"
        || manifest.artifact_digest != identity.artifact_digest
        || !manifest.operation_name.starts_with("external.")
    {
        bail!("CAPABILITY_MANIFEST_MISMATCH");
    }
    if Sha256Digest::compute(&serde_json::to_vec(manifest)?).as_str() != identity.manifest_digest {
        bail!("CAPABILITY_MANIFEST_DIGEST_MISMATCH");
    }
    Ok(())
}

pub(crate) fn capability_specs(
    conn: &rusqlite::Connection,
    identity: &TrustedDecisionIdentity,
    manifest: &HarnessManifest,
) -> Result<Vec<OperationSpec>> {
    let snapshot =
        super::JournalStore::load_snapshot_from_conn(conn, &identity.expected_source_snapshot_id)?;
    if snapshot.lookup(&manifest.operation_name).is_some() {
        bail!("CAPABILITY_ALREADY_REGISTERED");
    }
    let mut specs = snapshot.operations;
    specs.push(OperationSpec {
        name: manifest.operation_name.clone(),
        risk: Risk::ReadOnly,
        description: manifest.description.clone(),
        parameters: manifest.input_schema.clone(),
        idempotent: manifest.idempotent,
        binding_kind: BindingKind::External,
        binding_key: manifest.manifest_id.clone(),
    });
    Ok(specs)
}

pub(crate) fn append_approval_event(
    tx: &Transaction<'_>,
    binding: &Binding,
    identity: &TrustedDecisionIdentity,
) -> Result<()> {
    super::queue::append_event_tx(
        tx,
        JournalEventKind::CapabilityChangeApproved,
        Some(&RunId(binding.origin_run.clone())),
        Some(&SessionId(binding.origin_session.clone())),
        Some(&identity.proposal_id),
        json!({"proposal_id":identity.proposal_id,"approval_id":identity.approval_id,
               "decision_id":identity.decision_id,"decided_by":identity.principal_id}),
    )?;
    Ok(())
}

pub(crate) fn append_approved_events(
    tx: &Transaction<'_>,
    binding: &Binding,
    identity: &TrustedDecisionIdentity,
    snapshot: &str,
    deployment: &str,
    operation: &str,
) -> Result<()> {
    append_approval_event(tx, binding, identity)?;
    let run = RunId(binding.origin_run.clone());
    let session = SessionId(binding.origin_session.clone());
    super::queue::append_event_tx(
        tx,
        JournalEventKind::RegistrySnapshotActivated,
        Some(&run),
        Some(&session),
        Some(&identity.decision_id),
        json!({"action":"trusted_capability_activation","operation":operation,
            "previous_snapshot_id":identity.expected_source_snapshot_id,
            "new_snapshot_id":snapshot,"decision_id":identity.decision_id,
            "host_deployment_id":deployment}),
    )?;
    super::queue::append_event_tx(
        tx,
        JournalEventKind::CapabilityChangeActivated,
        Some(&run),
        Some(&session),
        Some(&identity.proposal_id),
        json!({"proposal_id":identity.proposal_id,"approval_id":identity.approval_id,
            "decision_id":identity.decision_id,"new_snapshot_id":snapshot,
            "host_deployment_id":deployment}),
    )?;
    Ok(())
}

pub(crate) fn append_grant_event(
    tx: &Transaction<'_>,
    binding: &Binding,
    identity: &TrustedDecisionIdentity,
    grant_id: &str,
    snapshot: &str,
    operation: &str,
) -> Result<()> {
    super::queue::append_event_tx(
        tx,
        JournalEventKind::ExternalOperationGranted,
        Some(&RunId(binding.origin_run.clone())),
        Some(&SessionId(binding.origin_session.clone())),
        Some(grant_id),
        json!({"grant_id":grant_id,"operation":operation,
            "grantee_principal_id":identity.principal_id,"channel":"Feishu",
            "conversation_kind":"p2p","scope":"principal_channel","risk":"ReadOnly",
            "snapshot_id":snapshot,"decision_id":identity.decision_id}),
    )?;
    Ok(())
}

pub(crate) fn expire_trusted_approval(
    conn: &mut rusqlite::Connection,
    approval_id: &str,
    expected_agent: &crate::domain::AgentId,
) -> Result<bool> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let row: Option<(String, String, String, String, String, String, String)> = tx
        .query_row(
            "SELECT a.proposal_id,a.status,p.status,p.target_agent_id,a.expires_at,
                    p.origin_run_id,p.origin_session_id
             FROM capability_governance_approvals a
             JOIN capability_change_proposals p ON p.proposal_id=a.proposal_id
             WHERE a.approval_id=?1",
            params![approval_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((proposal_id, approval_status, proposal_status, agent, expires, run, session)) = row
    else {
        bail!("TRUSTED_APPROVAL_NOT_FOUND");
    };
    if agent != expected_agent.0 {
        bail!("TRUSTED_APPROVAL_AGENT_MISMATCH");
    }
    if approval_status != "Pending" || proposal_status != "PendingApproval" {
        return Ok(false);
    }
    let expires_at = chrono::DateTime::parse_from_rfc3339(&expires)?.with_timezone(&Utc);
    if Utc::now() < expires_at {
        return Ok(false);
    }
    let now = Utc::now().to_rfc3339();
    let changed = tx.execute(
        "UPDATE capability_governance_approvals
         SET status='Expired',decided_at=?1,decided_by='kernel_expiry'
         WHERE approval_id=?2 AND status='Pending'",
        params![now, approval_id],
    )?;
    let proposal_changed = tx.execute(
        "UPDATE capability_change_proposals
         SET status='Expired',decided_at=?1,decided_by='kernel_expiry',
             decision_reason='approval_expired'
         WHERE proposal_id=?2 AND status='PendingApproval'",
        params![now, proposal_id],
    )?;
    if changed != 1 || proposal_changed != 1 {
        bail!("APPROVAL_EXPIRY_CONFLICT");
    }
    super::queue::append_event_tx(
        &tx,
        JournalEventKind::CapabilityChangeExpired,
        Some(&RunId(run)),
        Some(&SessionId(session)),
        Some(&proposal_id),
        json!({"proposal_id":proposal_id,"approval_id":approval_id,
               "decided_by":"kernel_expiry","reason":"approval_expired"}),
    )?;
    tx.commit()?;
    Ok(true)
}
