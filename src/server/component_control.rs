//! Owner-gated Kernel control plane for managed service components.

use super::capability_routes::CapabilityRouteError;
use super::deployment_harness_client::{
    is_definitive_rejection, DeploymentHarnessController, HttpDeploymentHarnessClient,
};
use crate::config::KernelConfig;
use crate::domain::{ComponentControlIntent, DEPLOYMENT_PROTOCOL};
use crate::journal::JournalStore;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlBody {
    principal_id: String,
    decision_nonce: String,
    expected_component_snapshot_id: String,
    expected_deployment_id: String,
}

pub(crate) fn handle(
    journal: &JournalStore,
    config: &KernelConfig,
    component_id: &str,
    action: &str,
    body: &Value,
) -> Result<Value> {
    let input: ControlBody = serde_json::from_value(body.clone())
        .map_err(|_| CapabilityRouteError::InvalidRequest("invalid_component_control".into()))?;
    let owner = config
        .feishu_coding_owner_id
        .as_deref()
        .ok_or_else(|| CapabilityRouteError::Forbidden("component_owner_not_configured".into()))?;
    let mut intent = ComponentControlIntent {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        decision_id: String::new(),
        decision_nonce: input.decision_nonce,
        principal_id: input.principal_id,
        component_id: component_id.into(),
        action: action.into(),
        expected_component_snapshot_id: input.expected_component_snapshot_id,
        expected_deployment_id: input.expected_deployment_id,
    };
    intent.decision_id = intent.expected_decision_id();
    intent
        .validate()
        .map_err(|_| CapabilityRouteError::InvalidRequest("invalid_component_control".into()))?;
    journal
        .record_component_control_intent(&intent, owner)
        .map_err(map_control_error)?;
    if let Some(result) = journal
        .replay_component_control(&intent)
        .map_err(map_control_error)?
    {
        return Ok(control_response(&intent, result));
    }

    let client = HttpDeploymentHarnessClient::from_env()
        .map_err(|_| CapabilityRouteError::Internal("deployment_harness_unavailable".into()))?;
    let receipt = match client.control(&intent) {
        Ok(receipt) => receipt,
        Err(error) if is_definitive_rejection(&error) => {
            journal
                .fail_component_control_intent(&intent.decision_id)
                .map_err(map_control_error)?;
            return Err(CapabilityRouteError::Conflict("component_control_rejected".into()).into());
        }
        Err(_) => {
            return Err(
                CapabilityRouteError::Internal("component_control_effect_uncertain".into()).into(),
            )
        }
    };
    let result = journal
        .settle_component_control_atomic(&intent, &receipt)
        .map_err(map_control_error)?;
    Ok(control_response(&intent, result))
}

fn control_response(
    intent: &ComponentControlIntent,
    result: crate::journal::component_control::ComponentControlResult,
) -> Value {
    json!({
        "ok": true,
        "decision_id": intent.decision_id,
        "receipt_id": result.receipt_id,
        "action": intent.action,
        "component_id": result.component.component_id,
        "component_version": result.component.version,
        "component_status": result.component.status,
        "component_url": result.component.endpoint,
        "deployment_id": result.component.deployment_id,
        "component_snapshot_id": result.target_snapshot_id,
        "replayed": result.replayed,
    })
}

pub(crate) fn observe(journal: &JournalStore, component_id: &str) -> Result<Value> {
    if component_id.is_empty() || component_id.contains('/') || component_id.len() > 128 {
        return Err(CapabilityRouteError::InvalidRequest("invalid_component_id".into()).into());
    }
    let snapshot_id = journal.current_component_snapshot_id().map_err(internal)?;
    let snapshot = journal
        .load_component_registry_snapshot(&snapshot_id)
        .map_err(internal)?;
    let component = snapshot
        .lookup(component_id)
        .ok_or_else(|| CapabilityRouteError::NotFound("component_not_found".into()))?;
    Ok(json!({
        "ok": true,
        "component_snapshot_id": snapshot_id,
        "component": component,
    }))
}

fn map_control_error(error: anyhow::Error) -> anyhow::Error {
    let text = error.to_string();
    if text.contains("OWNER_MISMATCH") {
        CapabilityRouteError::Forbidden("component_owner_mismatch".into()).into()
    } else if text.contains("NOT_REGISTERED") || text.contains("TARGET_UNTRUSTED") {
        CapabilityRouteError::NotFound("component_not_found".into()).into()
    } else if text.contains("CONFLICT")
        || text.contains("STATE_INVALID")
        || text.contains("IN_FLIGHT")
        || text.contains("TERMINAL")
    {
        CapabilityRouteError::Conflict("component_state_conflict".into()).into()
    } else if text.contains("INVALID") || text.contains("NOT_RECORDED") {
        CapabilityRouteError::InvalidRequest("invalid_component_control".into()).into()
    } else {
        internal(error)
    }
}

fn internal(error: impl std::fmt::Display) -> anyhow::Error {
    CapabilityRouteError::Internal(format!("{error}")).into()
}
