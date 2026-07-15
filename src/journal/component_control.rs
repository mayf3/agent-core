//! Atomic Kernel governance for managed-component disable and rollback.

use crate::domain::{
    ComponentControlIntent, ComponentControlReceipt, ComponentStatus, DeploymentReceipt,
    JournalEventKind, RegisteredComponent,
};
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentControlResult {
    pub target_snapshot_id: String,
    pub receipt_id: String,
    pub component: RegisteredComponent,
    pub replayed: bool,
}

impl super::JournalStore {
    pub fn record_component_control_intent(
        &self,
        intent: &ComponentControlIntent,
        expected_owner_open_id: &str,
    ) -> Result<()> {
        intent.validate()?;
        if intent.principal_id != format!("feishu:open_id:{expected_owner_open_id}") {
            bail!("COMPONENT_CONTROL_OWNER_MISMATCH");
        }
        let payload = serde_json::to_string(intent)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((existing, status)) = tx
            .query_row(
                "SELECT payload_json,status FROM component_control_intents WHERE decision_id=?1",
                params![intent.decision_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if existing != payload {
                bail!("COMPONENT_CONTROL_DECISION_CONFLICT");
            }
            if status == "failed" {
                bail!("COMPONENT_CONTROL_DECISION_TERMINAL");
            }
            tx.commit()?;
            return Ok(());
        }
        let pending_control: i64 = tx.query_row(
            "SELECT COUNT(*) FROM component_control_intents
             WHERE component_id=?1 AND status='pending'",
            params![intent.component_id],
            |row| row.get(0),
        )?;
        let pending_deployment: i64 = tx.query_row(
            "SELECT COUNT(*) FROM component_deployment_intents i
             JOIN capability_change_approvals a ON a.proposal_id=i.proposal_id
             WHERE i.component_id=?1 AND a.status='Pending'",
            params![intent.component_id],
            |row| row.get(0),
        )?;
        if pending_control != 0 || pending_deployment != 0 {
            bail!("COMPONENT_CONTROL_EFFECT_IN_FLIGHT");
        }
        let active = active_component(&tx, intent)?;
        if intent.action == "rollback" && active.status != ComponentStatus::Healthy {
            bail!("COMPONENT_ROLLBACK_STATE_INVALID");
        }
        if intent.action == "disable"
            && !matches!(
                active.status,
                ComponentStatus::Healthy | ComponentStatus::RolledBack
            )
        {
            bail!("COMPONENT_DISABLE_STATE_INVALID");
        }
        tx.execute(
            "INSERT INTO component_control_intents
             (decision_id,component_id,action,principal_id,expected_snapshot_id,
              expected_deployment_id,status,payload_json,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,'pending',?7,?8)",
            params![
                intent.decision_id,
                intent.component_id,
                intent.action,
                intent.principal_id,
                intent.expected_component_snapshot_id,
                intent.expected_deployment_id,
                payload,
                Utc::now().to_rfc3339(),
            ],
        )?;
        super::queue::append_event_tx(
            &tx,
            JournalEventKind::ComponentControlIntentRecorded,
            None,
            None,
            Some(&intent.decision_id),
            json!({
                "decision_id": intent.decision_id,
                "component_id": intent.component_id,
                "action": intent.action,
                "expected_snapshot_id": intent.expected_component_snapshot_id,
                "expected_deployment_id": intent.expected_deployment_id,
                "principal_id": intent.principal_id,
            }),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn replay_component_control(
        &self,
        intent: &ComponentControlIntent,
    ) -> Result<Option<ComponentControlResult>> {
        intent.validate()?;
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let recorded: String = conn
            .query_row(
                "SELECT payload_json FROM component_control_intents WHERE decision_id=?1",
                params![intent.decision_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow!("COMPONENT_CONTROL_INTENT_NOT_RECORDED"))?;
        if recorded != serde_json::to_string(intent)? {
            bail!("COMPONENT_CONTROL_INTENT_CONFLICT");
        }
        let Some((target_snapshot_id, payload)) = conn
            .query_row(
                "SELECT target_snapshot_id,payload_json FROM component_control_receipts
                 WHERE decision_id=?1",
                params![intent.decision_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let receipt: ComponentControlReceipt = serde_json::from_str(&payload)
            .map_err(|_| anyhow!("COMPONENT_CONTROL_RESULT_CORRUPT"))?;
        receipt
            .validate_for(intent)
            .map_err(|_| anyhow!("COMPONENT_CONTROL_RESULT_CORRUPT"))?;
        let snapshot = super::component_registry::load_snapshot(&conn, &target_snapshot_id)?;
        let component = snapshot
            .lookup(&intent.component_id)
            .cloned()
            .ok_or_else(|| anyhow!("COMPONENT_CONTROL_RESULT_CORRUPT"))?;
        Ok(Some(ComponentControlResult {
            target_snapshot_id,
            receipt_id: receipt.receipt_id,
            component,
            replayed: true,
        }))
    }

    pub fn fail_component_control_intent(&self, decision_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let changed = conn.execute(
            "UPDATE component_control_intents SET status='failed'
             WHERE decision_id=?1 AND status='pending'",
            params![decision_id],
        )?;
        if changed != 1 {
            bail!("COMPONENT_CONTROL_DECISION_CONFLICT");
        }
        Ok(())
    }

    pub fn settle_component_control_atomic(
        &self,
        intent: &ComponentControlIntent,
        receipt: &ComponentControlReceipt,
    ) -> Result<ComponentControlResult> {
        intent.validate()?;
        receipt.validate_for(intent)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_recorded_intent(&tx, intent)?;
        if let Some((target_snapshot_id, payload)) = tx
            .query_row(
                "SELECT target_snapshot_id,payload_json FROM component_control_receipts
                 WHERE decision_id=?1",
                params![intent.decision_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if payload != serde_json::to_string(receipt)? {
                bail!("COMPONENT_CONTROL_RECEIPT_CONFLICT");
            }
            let snapshot = super::component_registry::load_snapshot(&tx, &target_snapshot_id)?;
            let component = snapshot
                .lookup(&intent.component_id)
                .cloned()
                .ok_or_else(|| anyhow!("COMPONENT_CONTROL_RESULT_CORRUPT"))?;
            tx.commit()?;
            return Ok(ComponentControlResult {
                target_snapshot_id,
                receipt_id: receipt.receipt_id.clone(),
                component,
                replayed: true,
            });
        }

        let active = active_component(&tx, intent)?;
        let mut target = if intent.action == "disable" {
            if receipt.deployment_id != active.deployment_id
                || receipt.artifact_digest != active.artifact_digest
                || receipt.version != active.version
            {
                bail!("COMPONENT_DISABLE_RECEIPT_BINDING_MISMATCH");
            }
            active.clone()
        } else {
            validate_rollback_target(&tx, &active, receipt)?;
            historical_component(&tx, receipt)?
                .ok_or_else(|| anyhow!("COMPONENT_ROLLBACK_TARGET_UNTRUSTED"))?
        };
        target.endpoint = receipt.endpoint.clone();
        target.status = if intent.action == "disable" {
            ComponentStatus::Disabled
        } else {
            ComponentStatus::RolledBack
        };

        let source =
            super::component_registry::load_snapshot(&tx, &intent.expected_component_snapshot_id)?;
        let mut components = source.components;
        components.retain(|component| component.component_id != intent.component_id);
        components.push(target.clone());
        let target_snapshot_id = super::component_registry::persist_snapshot(&tx, &components)?;
        let changed = tx.execute(
            "UPDATE component_registry_state
             SET active_snapshot_id=?1,version=version+1,updated_at=?2
             WHERE singleton_id=1 AND active_snapshot_id=?3",
            params![
                target_snapshot_id,
                Utc::now().to_rfc3339(),
                intent.expected_component_snapshot_id,
            ],
        )?;
        if changed != 1 {
            bail!("COMPONENT_CONTROL_SNAPSHOT_CONFLICT");
        }
        let payload = serde_json::to_string(receipt)?;
        tx.execute(
            "INSERT INTO component_control_receipts
             (receipt_id,decision_id,component_id,action,target_snapshot_id,payload_json,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                receipt.receipt_id,
                receipt.decision_id,
                receipt.component_id,
                receipt.action,
                target_snapshot_id,
                payload,
                Utc::now().to_rfc3339(),
            ],
        )?;
        let changed = tx.execute(
            "UPDATE component_control_intents SET status='succeeded'
             WHERE decision_id=?1 AND status='pending'",
            params![intent.decision_id],
        )?;
        if changed != 1 {
            bail!("COMPONENT_CONTROL_DECISION_CONFLICT");
        }
        append_settlement_events(&tx, intent, receipt, &target_snapshot_id)?;
        tx.commit()?;
        Ok(ComponentControlResult {
            target_snapshot_id,
            receipt_id: receipt.receipt_id.clone(),
            component: target,
            replayed: false,
        })
    }
}

fn validate_rollback_target(
    tx: &Transaction<'_>,
    active: &RegisteredComponent,
    receipt: &ComponentControlReceipt,
) -> Result<()> {
    let payload: String = tx
        .query_row(
            "SELECT payload_json FROM component_deployment_receipts WHERE receipt_id=?1",
            params![active.deployment_receipt_id],
            |row| row.get(0),
        )
        .map_err(|_| anyhow!("COMPONENT_ROLLBACK_LINEAGE_MISSING"))?;
    let deployment: DeploymentReceipt = serde_json::from_str(&payload)
        .map_err(|_| anyhow!("COMPONENT_ROLLBACK_LINEAGE_INVALID"))?;
    if deployment.receipt_id != active.deployment_receipt_id
        || deployment.deployment_id != active.deployment_id
        || deployment.artifact_digest != active.artifact_digest
        || deployment.version != active.version
        || deployment.previous_artifact_digest.as_deref() != Some(receipt.artifact_digest.as_str())
        || receipt.deployment_id == active.deployment_id
    {
        bail!("COMPONENT_ROLLBACK_LINEAGE_INVALID");
    }
    Ok(())
}

fn active_component(
    tx: &Transaction<'_>,
    intent: &ComponentControlIntent,
) -> Result<RegisteredComponent> {
    let active_snapshot: String = tx.query_row(
        "SELECT active_snapshot_id FROM component_registry_state WHERE singleton_id=1",
        [],
        |row| row.get(0),
    )?;
    if active_snapshot != intent.expected_component_snapshot_id {
        bail!("COMPONENT_CONTROL_SNAPSHOT_CONFLICT");
    }
    let snapshot = super::component_registry::load_snapshot(tx, &active_snapshot)?;
    let component = snapshot
        .lookup(&intent.component_id)
        .cloned()
        .ok_or_else(|| anyhow!("COMPONENT_NOT_REGISTERED"))?;
    if component.deployment_id != intent.expected_deployment_id {
        bail!("COMPONENT_CONTROL_DEPLOYMENT_CONFLICT");
    }
    Ok(component)
}

fn historical_component(
    tx: &Transaction<'_>,
    receipt: &ComponentControlReceipt,
) -> Result<Option<RegisteredComponent>> {
    let snapshot_id = tx
        .query_row(
            "SELECT e.snapshot_id FROM component_registry_entries e
             JOIN component_registry_snapshots s ON s.snapshot_id=e.snapshot_id
             WHERE e.component_id=?1 AND e.artifact_digest=?2 AND e.version=?3
               AND e.deployment_id=?4
             ORDER BY s.created_at DESC LIMIT 1",
            params![
                receipt.component_id,
                receipt.artifact_digest,
                receipt.version,
                receipt.deployment_id,
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(snapshot_id) = snapshot_id else {
        return Ok(None);
    };
    Ok(super::component_registry::load_snapshot(tx, &snapshot_id)?
        .lookup(&receipt.component_id)
        .cloned())
}

fn validate_recorded_intent(tx: &Transaction<'_>, intent: &ComponentControlIntent) -> Result<()> {
    let payload: String = tx
        .query_row(
            "SELECT payload_json FROM component_control_intents WHERE decision_id=?1",
            params![intent.decision_id],
            |row| row.get(0),
        )
        .map_err(|_| anyhow!("COMPONENT_CONTROL_INTENT_NOT_RECORDED"))?;
    if payload != serde_json::to_string(intent)? {
        bail!("COMPONENT_CONTROL_INTENT_CONFLICT");
    }
    Ok(())
}

fn append_settlement_events(
    tx: &Transaction<'_>,
    intent: &ComponentControlIntent,
    receipt: &ComponentControlReceipt,
    target_snapshot_id: &str,
) -> Result<()> {
    super::queue::append_event_tx(
        tx,
        JournalEventKind::ComponentControlReceiptRecorded,
        None,
        None,
        Some(&receipt.receipt_id),
        json!({
            "receipt_id": receipt.receipt_id,
            "decision_id": intent.decision_id,
            "component_id": intent.component_id,
            "action": intent.action,
            "deployment_id": receipt.deployment_id,
            "artifact_digest": receipt.artifact_digest,
            "version": receipt.version,
            "status": receipt.status,
            "health_status": receipt.health_status,
            "target_snapshot_id": target_snapshot_id,
        }),
    )?;
    let kind = if intent.action == "disable" {
        JournalEventKind::ComponentDisabled
    } else {
        JournalEventKind::ComponentRolledBack
    };
    super::queue::append_event_tx(
        tx,
        kind,
        None,
        None,
        Some(&intent.component_id),
        json!({
            "decision_id": intent.decision_id,
            "receipt_id": receipt.receipt_id,
            "component_id": intent.component_id,
            "previous_snapshot_id": intent.expected_component_snapshot_id,
            "new_snapshot_id": target_snapshot_id,
            "deployment_id": receipt.deployment_id,
            "artifact_digest": receipt.artifact_digest,
            "version": receipt.version,
        }),
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/component_control.rs"]
mod tests;
