//! Replay-safe trusted deployment transaction for managed service components.

use super::activation_core::Binding;
use super::trusted_capability_activation::{TrustedDecisionIdentity, TrustedDecisionResult};
use crate::domain::{
    ComponentStatus, DeploymentIntent, DeploymentReceipt, JournalEventKind, RegisteredComponent,
    RunId, ServiceManifest, SessionId,
};
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::json;

impl super::JournalStore {
    /// Durably record the exact effect intent before the external Deployment
    /// Harness is contacted. Repeating the same identity is idempotent;
    /// conflicting reuse fails closed.
    pub fn record_trusted_service_deployment_intent(
        &self,
        identity: &TrustedDecisionIdentity,
        intent: &DeploymentIntent,
        manifest: &ServiceManifest,
        expected_agent: &crate::domain::AgentId,
    ) -> Result<()> {
        manifest.validate()?;
        intent.validate()?;
        if intent.proposal_id != identity.proposal_id
            || intent.decision_id != identity.decision_id
            || intent.service_manifest_digest != identity.manifest_digest
            || intent.artifact_digest != identity.artifact_digest
            || intent.expected_version != manifest.version
            || intent.artifact_digest != manifest.artifact_digest
        {
            bail!("DEPLOYMENT_INTENT_BINDING_MISMATCH");
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
        super::trusted_capability_activation::validate_pending(&tx, &binding, identity)?;
        if binding.link_operation != manifest.component_id {
            bail!("SERVICE_COMPONENT_BINDING_MISMATCH");
        }
        // Record the trusted decision journal event atomically with the
        // deployment intent. The approval table row stays Pending until
        // activation completes — the CHECK constraint requires snapshot
        // and deployment IDs for the Approved state.  The journal event
        // serves as the authoritative audit trail of the decision and
        // is committed in the same SQLite transaction.
        super::activation_core::append_approval_event(&tx, &binding, identity)?;
	        let active_component_snapshot: String = tx.query_row(
	            "SELECT active_snapshot_id FROM component_registry_state WHERE singleton_id=1",
	            [],
	            |row| row.get(0),
	        )?;
        let components = super::component_registry::load_snapshot(&tx, &active_component_snapshot)?;
        if let Some(current) = components.lookup(&manifest.component_id) {
            if compare_version(&manifest.version, &current.version) != std::cmp::Ordering::Greater {
                bail!("SERVICE_VERSION_NOT_MONOTONIC");
            }
        }
        let payload = serde_json::to_string(intent)?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT component_id,payload_json FROM component_deployment_intents
                 WHERE intent_id=?1",
                params![intent.intent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing_component, existing_payload)) = existing {
            if existing_component != manifest.component_id || existing_payload != payload {
                bail!("DEPLOYMENT_INTENT_CONFLICT");
            }
            tx.commit()?;
            return Ok(());
        }
        let in_flight: i64 = tx.query_row(
            "SELECT COUNT(*) FROM component_deployment_intents i
             LEFT JOIN component_deployment_receipts r
               ON r.proposal_id = i.proposal_id AND r.component_id = i.component_id
             WHERE i.component_id=?1 AND r.receipt_id IS NULL AND i.intent_id != ?2",
            params![manifest.component_id, intent.intent_id],
            |row| row.get(0),
        )?;
        let pending_control: i64 = tx.query_row(
            "SELECT COUNT(*) FROM component_control_intents
             WHERE component_id=?1 AND status='pending'",
            params![manifest.component_id],
            |row| row.get(0),
        )?;
        if in_flight != 0 || pending_control != 0 {
            bail!("SERVICE_COMPONENT_EFFECT_IN_FLIGHT");
        }
        tx.execute(
            "INSERT INTO component_deployment_intents
             (intent_id,invocation_id,proposal_id,decision_id,component_id,manifest_digest,
              artifact_digest,expected_version,payload_json,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                intent.intent_id,
                intent.invocation_id,
                intent.proposal_id,
                intent.decision_id,
                manifest.component_id,
                intent.service_manifest_digest,
                intent.artifact_digest,
                intent.expected_version,
                payload,
                Utc::now().to_rfc3339(),
            ],
        )?;
        append_intent_event(&tx, &binding, identity, intent, &manifest.component_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically bind a healthy deployment receipt, publish a new immutable
    /// component snapshot, and settle the Proposal/Approval.
    pub fn activate_trusted_service_atomic(
        &self,
        identity: &TrustedDecisionIdentity,
        intent: &DeploymentIntent,
        manifest: &ServiceManifest,
        receipt: &DeploymentReceipt,
        expected_agent: &crate::domain::AgentId,
    ) -> Result<TrustedDecisionResult> {
        manifest.validate()?;
        intent.validate()?;
        receipt.validate_for(intent, &manifest.component_id)?;
        if manifest.manifest_id.is_empty()
            || intent.service_manifest_digest != identity.manifest_digest
            || manifest.artifact_digest != identity.artifact_digest
        {
            bail!("SERVICE_ACTIVATION_BINDING_MISMATCH");
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
            let result = super::trusted_capability_activation::replay_result(
                &binding, identity, "Approved",
            )?;
            if result.host_deployment_id.as_deref() != Some(&receipt.deployment_id) {
                bail!("APPROVAL_DECISION_CONFLICT");
            }
            return Ok(result);
        }
        super::trusted_capability_activation::validate_pending(&tx, &binding, identity)?;
        if binding.link_operation != manifest.component_id {
            bail!("SERVICE_COMPONENT_BINDING_MISMATCH");
        }
        validate_recorded_intent(&tx, intent, &manifest.component_id)?;

        let (source_snapshot_id, component_version): (String, i64) = tx.query_row(
            "SELECT active_snapshot_id,version FROM component_registry_state WHERE singleton_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let source = super::component_registry::load_snapshot(&tx, &source_snapshot_id)?;
        if let Some(current) = source.lookup(&manifest.component_id) {
            if compare_version(&manifest.version, &current.version) != std::cmp::Ordering::Greater {
                bail!("SERVICE_VERSION_NOT_MONOTONIC");
            }
            if receipt.previous_artifact_digest.as_deref() != Some(&current.artifact_digest) {
                bail!("SERVICE_PREVIOUS_ARTIFACT_MISMATCH");
            }
        } else if receipt.previous_artifact_digest.is_some() {
            bail!("SERVICE_PREVIOUS_ARTIFACT_MISMATCH");
        }

        let receipt_payload = serde_json::to_string(receipt)?;
        tx.execute(
            "INSERT INTO component_deployment_receipts
             (receipt_id,deployment_id,invocation_id,proposal_id,decision_id,component_id,
              manifest_digest,artifact_digest,version,endpoint,health_status,log_ref,
              payload_json,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                receipt.receipt_id,
                receipt.deployment_id,
                receipt.invocation_id,
                receipt.proposal_id,
                receipt.decision_id,
                receipt.component_id,
                receipt.service_manifest_digest,
                receipt.artifact_digest,
                receipt.version,
                receipt.endpoint,
                receipt.health_status,
                receipt.log_ref,
                receipt_payload,
                Utc::now().to_rfc3339(),
            ],
        )?;
        let component = RegisteredComponent {
            component_id: manifest.component_id.clone(),
            kind: manifest.kind,
            manifest_id: manifest.manifest_id.clone(),
            manifest_digest: identity.manifest_digest.clone(),
            artifact_digest: manifest.artifact_digest.clone(),
            version: manifest.version.clone(),
            endpoint: receipt.endpoint.clone(),
            deployment_id: receipt.deployment_id.clone(),
            deployment_receipt_id: receipt.receipt_id.clone(),
            status: ComponentStatus::Healthy,
            required_contracts: manifest.required_contracts.clone(),
            requested_permissions: manifest.requested_permissions.clone(),
        };
        let mut components = source.components;
        components.retain(|current| current.component_id != component.component_id);
        components.push(component);
        let target_snapshot_id = super::component_registry::persist_snapshot(&tx, &components)?;
        let changed = tx.execute(
            "UPDATE component_registry_state SET active_snapshot_id=?1,version=?2,updated_at=?3
             WHERE singleton_id=1 AND version=?4 AND active_snapshot_id=?5",
            params![
                target_snapshot_id,
                component_version + 1,
                Utc::now().to_rfc3339(),
                component_version,
                source_snapshot_id,
            ],
        )?;
        if changed != 1 {
            bail!("COMPONENT_REGISTRY_ACTIVATION_CONFLICT");
        }

        let result = TrustedDecisionResult {
            decision_id: identity.decision_id.clone(),
            status: crate::domain::CapabilityApprovalStatus::Approved,
            activated_snapshot_id: Some(target_snapshot_id.clone()),
            host_deployment_id: Some(receipt.deployment_id.clone()),
            activation_error: None,
            replayed: false,
        };
        super::trusted_capability_activation::persist_terminal(
            &tx,
            &binding,
            identity,
            "Approved",
            "Activated",
            &result,
            Some(&target_snapshot_id),
            Some(&receipt.deployment_id),
            None,
        )?;
        append_activation_events(
            &tx,
            &binding,
            identity,
            intent,
            manifest,
            receipt,
            &source_snapshot_id,
            &target_snapshot_id,
        )?;
        tx.commit()?;
        Ok(result)
    }
}

fn validate_recorded_intent(
    tx: &rusqlite::Transaction<'_>,
    intent: &DeploymentIntent,
    component_id: &str,
) -> Result<()> {
    let row: (String, String) = tx
        .query_row(
            "SELECT component_id,payload_json FROM component_deployment_intents WHERE intent_id=?1",
            params![intent.intent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| anyhow::anyhow!("DEPLOYMENT_INTENT_NOT_RECORDED"))?;
    if row.0 != component_id || row.1 != serde_json::to_string(intent)? {
        bail!("DEPLOYMENT_INTENT_BINDING_MISMATCH");
    }
    Ok(())
}

fn compare_version(left: &str, right: &str) -> std::cmp::Ordering {
    let parse = |value: &str| {
        value
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(left).cmp(&parse(right))
}

fn append_intent_event(
    tx: &rusqlite::Transaction<'_>,
    binding: &Binding,
    identity: &TrustedDecisionIdentity,
    intent: &DeploymentIntent,
    component_id: &str,
) -> Result<()> {
    super::queue::append_event_tx(
        tx,
        JournalEventKind::DeploymentIntentRecorded,
        Some(&RunId(binding.origin_run.clone())),
        Some(&SessionId(binding.origin_session.clone())),
        Some(&intent.intent_id),
        json!({
            "intent_id":intent.intent_id,"invocation_id":intent.invocation_id,
            "proposal_id":identity.proposal_id,"decision_id":identity.decision_id,
            "component_id":component_id,"manifest_digest":intent.service_manifest_digest,
            "artifact_digest":intent.artifact_digest,"version":intent.expected_version,
        }),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_activation_events(
    tx: &rusqlite::Transaction<'_>,
    binding: &Binding,
    identity: &TrustedDecisionIdentity,
    intent: &DeploymentIntent,
    manifest: &ServiceManifest,
    receipt: &DeploymentReceipt,
    source_snapshot_id: &str,
    target_snapshot_id: &str,
) -> Result<()> {
    super::activation_core::append_approval_event(tx, binding, identity)?;
    let run = RunId(binding.origin_run.clone());
    let session = SessionId(binding.origin_session.clone());
    super::queue::append_event_tx(
        tx,
        JournalEventKind::DeploymentReceiptRecorded,
        Some(&run),
        Some(&session),
        Some(&receipt.receipt_id),
        json!({
            "receipt_id":receipt.receipt_id,"deployment_id":receipt.deployment_id,
            "intent_id":intent.intent_id,"component_id":manifest.component_id,
            "artifact_digest":receipt.artifact_digest,"version":receipt.version,
            "status":receipt.status,"health_status":receipt.health_status,
            "endpoint":receipt.endpoint,"log_ref":receipt.log_ref,
        }),
    )?;
    super::queue::append_event_tx(
        tx,
        JournalEventKind::ComponentRegistered,
        Some(&run),
        Some(&session),
        Some(&manifest.component_id),
        json!({
            "component_id":manifest.component_id,"kind":manifest.kind,
            "manifest_id":manifest.manifest_id,"artifact_digest":manifest.artifact_digest,
            "version":manifest.version,"deployment_id":receipt.deployment_id,
            "previous_snapshot_id":source_snapshot_id,"new_snapshot_id":target_snapshot_id,
        }),
    )?;
    super::queue::append_event_tx(
        tx,
        JournalEventKind::CapabilityChangeActivated,
        Some(&run),
        Some(&session),
        Some(&identity.proposal_id),
        json!({
            "proposal_id":identity.proposal_id,"approval_id":identity.approval_id,
            "decision_id":identity.decision_id,"component_id":manifest.component_id,
            "new_component_snapshot_id":target_snapshot_id,
            "host_deployment_id":receipt.deployment_id,"endpoint":receipt.endpoint,
        }),
    )?;
    Ok(())
}

/// Check whether a deployment intent exists for the given proposal without
/// a corresponding receipt — i.e., the background deployment is still in
/// flight. Used by the async approval path to prevent duplicate threads.
pub fn intent_exists_without_receipt(
    journal: &super::JournalStore,
    proposal_id: &str,
    manifest_digest: &str,
) -> Result<bool> {
    let count: i64 = journal.conn.lock().unwrap().query_row(
        "SELECT COUNT(*) FROM component_deployment_intents i
         LEFT JOIN component_deployment_receipts r
           ON r.intent_id = i.intent_id AND r.proposal_id = i.proposal_id
         WHERE i.proposal_id=?1 AND i.manifest_digest=?2 AND r.receipt_id IS NULL",
        params![proposal_id, manifest_digest],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count > 0)
}

#[cfg(test)]
#[path = "tests/trusted_service_activation.rs"]
mod tests;

