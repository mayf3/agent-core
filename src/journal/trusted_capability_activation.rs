//! Kernel-owned, replay-safe decision transaction for trusted capability Proposals.

use crate::domain::{AgentId, CapabilityApprovalStatus, JournalEventKind, RunId, SessionId};
use crate::harness::manifest::HarnessManifest;
use crate::journal::grant_ops::CreateGrantParams;
use crate::registry::snapshot::compute_snapshot_id;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::activation_core::Binding;

#[derive(Clone, Debug)]
pub struct TrustedDecisionIdentity {
    pub proposal_id: String,
    pub approval_id: String,
    pub decision_nonce: String,
    pub principal_id: String,
    pub expected_source_snapshot_id: String,
    pub candidate_digest: String,
    pub artifact_digest: String,
    pub manifest_digest: String,
    pub decision_id: String,
    pub payload_digest: String,
}

#[derive(Clone, Debug)]
pub struct TrustedHostDeployment {
    pub deployment_id: String,
    pub target_snapshot_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedDecisionResult {
    pub decision_id: String,
    pub status: CapabilityApprovalStatus,
    pub activated_snapshot_id: Option<String>,
    pub host_deployment_id: Option<String>,
    pub activation_error: Option<String>,
    pub replayed: bool,
}

impl super::JournalStore {
    /// Return a durable terminal result only when every caller-supplied replay
    /// identity field matches. Pending returns None; conflicting replays fail.
    pub fn replay_trusted_capability_decision(
        &self,
        identity: &TrustedDecisionIdentity,
        expected_agent: &AgentId,
        expected_terminal: CapabilityApprovalStatus,
    ) -> Result<Option<TrustedDecisionResult>> {
        let expected = match expected_terminal {
            CapabilityApprovalStatus::Approved => "Approved",
            CapabilityApprovalStatus::Rejected => "Rejected",
            CapabilityApprovalStatus::ActivationFailed => "ActivationFailed",
            _ => bail!("APPROVAL_REPLAY_TERMINAL_INVALID"),
        };
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let binding = super::trusted_capability_validation::load_validated_binding(
            &conn,
            identity,
            expected_agent,
        )?;
        if binding.approval_status == "Pending" {
            return Ok(None);
        }
        replay_result(&binding, identity, expected).map(Some)
    }

    /// Compute the S1 identity that must be sent to Capability Host. Activation
    /// recomputes it inside BEGIN IMMEDIATE, so this read is only preparation.
    pub fn trusted_capability_prospective_snapshot(
        &self,
        identity: &TrustedDecisionIdentity,
        manifest: &HarnessManifest,
        expected_agent: &AgentId,
    ) -> Result<String> {
        super::activation_core::validate_capability_manifest(manifest, identity)?;
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let binding = super::trusted_capability_validation::load_validated_binding(
            &conn,
            identity,
            expected_agent,
        )?;
        if binding.approval_status != "Pending" {
            let replay = replay_result(&binding, identity, "Approved")?;
            return replay
                .activated_snapshot_id
                .ok_or_else(|| anyhow::anyhow!("APPROVAL_REPLAY_RESULT_CORRUPT"));
        }
        validate_pending(&conn, &binding, identity)?;
        let specs = super::activation_core::capability_specs(&conn, identity, manifest)?;
        compute_snapshot_id(&specs)
    }

    pub fn activate_trusted_capability_atomic(
        &self,
        identity: &TrustedDecisionIdentity,
        manifest: &HarnessManifest,
        deployment: &TrustedHostDeployment,
        expected_agent: &AgentId,
    ) -> Result<TrustedDecisionResult> {
        super::activation_core::validate_capability_manifest(manifest, identity)?;
        if deployment.deployment_id.trim().is_empty() {
            bail!("HOST_DEPLOYMENT_ID_MISSING");
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let binding = super::trusted_capability_validation::load_validated_binding(
            &tx,
            identity,
            expected_agent,
        )?;
        if binding.approval_status != "Pending" {
            let result = replay_result(&binding, identity, "Approved")?;
            if result.host_deployment_id.as_deref() != Some(&deployment.deployment_id)
                || result.activated_snapshot_id.as_deref() != Some(&deployment.target_snapshot_id)
            {
                bail!("APPROVAL_DECISION_CONFLICT");
            }
            return Ok(result);
        }
        validate_decidable(&binding)?;
        let specs = super::activation_core::capability_specs(&tx, identity, manifest)?;
        let prospective = compute_snapshot_id(&specs)?;
        if deployment.target_snapshot_id != prospective {
            bail!("HOST_DEPLOYMENT_SNAPSHOT_MISMATCH");
        }

        self.register_harness_manifest_in_tx(&tx, manifest)
            .map_err(|error| anyhow::anyhow!("manifest_registration_failed:{error}"))?;
        let activation = super::activation_core::activate_registry_tx(
            &tx,
            &specs,
            &identity.expected_source_snapshot_id,
        )?;
        if activation.previous_snapshot_id != identity.expected_source_snapshot_id
            || activation.new_snapshot_id != deployment.target_snapshot_id
        {
            bail!("REGISTRY_ACTIVATION_IDENTITY_MISMATCH");
        }

        let grant = CreateGrantParams {
            operation: manifest.operation_name.clone(),
            grantee_principal_id: identity.principal_id.clone(),
            channel: "Feishu".into(),
            conversation_kind: "p2p".into(),
            scope: "principal_channel".into(),
            risk: "ReadOnly".into(),
            capability_id: Some(identity.proposal_id.clone()),
            snapshot_id: activation.new_snapshot_id.clone(),
            created_by_principal_id: Some(identity.principal_id.clone()),
            decision_reference: Some(identity.decision_id.clone()),
        };
        let (grant_id, grant_inserted) =
            super::grant_ops::create_external_operation_grant_tx(&tx, &grant)?;
        let result = TrustedDecisionResult {
            decision_id: identity.decision_id.clone(),
            status: CapabilityApprovalStatus::Approved,
            activated_snapshot_id: Some(activation.new_snapshot_id.clone()),
            host_deployment_id: Some(deployment.deployment_id.clone()),
            activation_error: None,
            replayed: false,
        };
        persist_terminal(
            &tx,
            &binding,
            identity,
            "Approved",
            "Activated",
            &result,
            Some(&activation.new_snapshot_id),
            Some(&deployment.deployment_id),
            None,
        )?;
        super::activation_core::append_approved_events(
            &tx,
            &binding,
            identity,
            &activation.new_snapshot_id,
            &deployment.deployment_id,
            &manifest.operation_name,
        )?;
        if grant_inserted {
            super::activation_core::append_grant_event(
                &tx,
                &binding,
                identity,
                &grant_id,
                &activation.new_snapshot_id,
                &manifest.operation_name,
            )?;
        }
        tx.commit()?;
        drop(conn);
        *self.current_snapshot_id.lock().unwrap() = Some(activation.new_snapshot_id);
        Ok(result)
    }

    pub fn reject_trusted_capability_atomic(
        &self,
        identity: &TrustedDecisionIdentity,
        expected_agent: &AgentId,
    ) -> Result<TrustedDecisionResult> {
        self.finish_without_activation(identity, expected_agent, "Rejected", None)
    }

    pub fn fail_trusted_activation_atomic(
        &self,
        identity: &TrustedDecisionIdentity,
        error_code: &str,
        expected_agent: &AgentId,
    ) -> Result<TrustedDecisionResult> {
        if error_code.is_empty()
            || error_code.len() > 120
            || !error_code
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            bail!("ACTIVATION_ERROR_CODE_INVALID");
        }
        self.finish_without_activation(
            identity,
            expected_agent,
            "ActivationFailed",
            Some(error_code),
        )
    }

    /// Atomically close an Approval whose TTL elapsed. This is safe to call
    /// from a periodic sweep: non-pending or not-yet-expired rows are no-ops.
    pub fn expire_trusted_capability_approval_atomic(
        &self,
        approval_id: &str,
        expected_agent: &AgentId,
    ) -> Result<bool> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        super::activation_core::expire_trusted_approval(&mut conn, approval_id, expected_agent)
    }

    fn finish_without_activation(
        &self,
        identity: &TrustedDecisionIdentity,
        expected_agent: &AgentId,
        terminal: &str,
        error_code: Option<&str>,
    ) -> Result<TrustedDecisionResult> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let binding = super::trusted_capability_validation::load_validated_binding(
            &tx,
            identity,
            expected_agent,
        )?;
        if binding.approval_status != "Pending" {
            return replay_result(&binding, identity, terminal);
        }
        validate_decidable(&binding)?;
        let status = if terminal == "Rejected" {
            CapabilityApprovalStatus::Rejected
        } else {
            CapabilityApprovalStatus::ActivationFailed
        };
        let result = TrustedDecisionResult {
            decision_id: identity.decision_id.clone(),
            status,
            activated_snapshot_id: None,
            host_deployment_id: None,
            activation_error: error_code.map(str::to_string),
            replayed: false,
        };
        persist_terminal(
            &tx, &binding, identity, terminal, terminal, &result, None, None, error_code,
        )?;
        let run = RunId(binding.origin_run.clone());
        let session = SessionId(binding.origin_session.clone());
        if terminal == "Rejected" {
            super::queue::append_event_tx(
                &tx,
                JournalEventKind::CapabilityChangeRejected,
                Some(&run),
                Some(&session),
                Some(&identity.proposal_id),
                json!({"proposal_id":identity.proposal_id,"decision_id":identity.decision_id,
                       "decided_by":identity.principal_id}),
            )?;
        } else {
            super::activation_core::append_approval_event(&tx, &binding, identity)?;
            super::queue::append_event_tx(
                &tx,
                JournalEventKind::CapabilityChangeActivationFailed,
                Some(&run),
                Some(&session),
                Some(&identity.proposal_id),
                json!({"proposal_id":identity.proposal_id,"decision_id":identity.decision_id,
                       "decided_by":identity.principal_id,"error_code":error_code}),
            )?;
        }
        tx.commit()?;
        Ok(result)
    }
}

pub(super) fn validate_pending(
    conn: &Connection,
    b: &Binding,
    i: &TrustedDecisionIdentity,
) -> Result<()> {
    validate_decidable(b)?;
    let active: String = conn.query_row(
        "SELECT active_snapshot_id FROM registry_state WHERE singleton_id=1",
        [],
        |r| r.get(0),
    )?;
    if active != i.expected_source_snapshot_id {
        bail!("SOURCE_REGISTRY_SNAPSHOT_CHANGED");
    }
    Ok(())
}

pub(super) fn validate_decidable(b: &Binding) -> Result<()> {
    if b.approval_status != "Pending" || b.proposal_status != "PendingApproval" {
        bail!("APPROVAL_NOT_PENDING");
    }
    let expires = DateTime::parse_from_rfc3339(&b.approval_expires_at)?.with_timezone(&Utc);
    if Utc::now() >= expires {
        bail!("APPROVAL_EXPIRED");
    }
    Ok(())
}

pub(super) fn replay_result(
    b: &Binding,
    i: &TrustedDecisionIdentity,
    expected: &str,
) -> Result<TrustedDecisionResult> {
    if b.approval_status != expected
        || b.decision_id.as_deref() != Some(&i.decision_id)
        || b.payload_digest.as_deref() != Some(&i.payload_digest)
        || b.decided_by.as_deref() != Some(&i.principal_id)
    {
        bail!("APPROVAL_DECISION_CONFLICT");
    }
    let mut result: TrustedDecisionResult = serde_json::from_str(
        b.result_json
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("APPROVAL_REPLAY_RESULT_CORRUPT"))?,
    )
    .map_err(|_| anyhow::anyhow!("APPROVAL_REPLAY_RESULT_CORRUPT"))?;
    if result.decision_id != i.decision_id
        || format!("{:?}", result.status) != expected
        || result.activated_snapshot_id != b.activated_snapshot
        || result.host_deployment_id != b.deployment_id
        || result.activation_error != b.activation_error
    {
        bail!("APPROVAL_REPLAY_RESULT_CORRUPT");
    }
    result.replayed = true;
    Ok(result)
}

pub(super) fn persist_terminal(
    tx: &rusqlite::Transaction<'_>,
    b: &Binding,
    i: &TrustedDecisionIdentity,
    approval_status: &str,
    proposal_status: &str,
    result: &TrustedDecisionResult,
    snapshot: Option<&str>,
    deployment: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let result_json = serde_json::to_string(result)?;
    let changed = tx.execute(
        "UPDATE capability_change_approvals SET status=?1,decision_id=?2,decision_payload_digest=?3,
         decision_result_json=?4,decided_at=?5,decided_by=?6,activated_snapshot_id=?7,
         host_deployment_id=?8,activation_error=?9 WHERE approval_id=?10 AND status='Pending'",
        params![approval_status,i.decision_id,i.payload_digest,result_json,now,i.principal_id,
            snapshot,deployment,error,i.approval_id])?;
    if changed != 1 {
        bail!("APPROVAL_NOT_PENDING");
    }
    let changed = tx.execute(
        "UPDATE capability_change_proposals SET status=?1,decided_at=?2,decided_by=?3,
         decision_reason=?4,activated_snapshot_id=?5,activation_error=?6
         WHERE proposal_id=?7 AND status='PendingApproval'",
        params![
            proposal_status,
            now,
            i.principal_id,
            approval_status,
            snapshot,
            error,
            i.proposal_id
        ],
    )?;
    if changed != 1 || b.proposal_status != "PendingApproval" {
        bail!("PROPOSAL_NOT_PENDING");
    }
    Ok(())
}
