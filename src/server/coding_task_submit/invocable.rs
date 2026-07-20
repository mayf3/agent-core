use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::Value;

pub fn invocable_manifest(
    request: &DevelopmentRequest,
    component: &Value,
    artifact_digest: &str,
) -> Result<HarnessManifest> {
    if request.target_kind != TargetKind::InvocableCapability
        || required_str(component, "schema_version")? != "component-artifact-v1"
        || required_str(component, "kind")? != "invocable_capability"
        || required_str(component, "component_id")? != request.name
        || required_str(component, "profile_id")? != request.build_profile
        || required_str(component, "contract_catalog_version")? != request.contract_catalog_version
        || required_str(component, "deployment_profile")? != request.deployment_profile
        || !string_set_matches(component, "required_contracts", &request.required_contracts)?
        || !string_set_matches(
            component,
            "requested_permissions",
            &request.requested_permissions,
        )?
    {
        bail!("COMPONENT_MANIFEST_IDENTITY_MISMATCH");
    }
    let capability = component
        .get("capability")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow::anyhow!("CAPABILITY_MANIFEST_MISSING"))?;
    if required_str(capability, "operation_name")? != request.name {
        bail!("CAPABILITY_OPERATION_MISMATCH");
    }
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "capability-host-v0".to_string(),
        artifact_digest: artifact_digest.to_string(),
        protocol_version: "external-harness-v1".to_string(),
        endpoint: "http://127.0.0.1:7300/execute".to_string(),
        operation_name: request.name.clone(),
        description: required_str(capability, "description")?.to_string(),
        input_schema: capability
            .get("input_schema")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_INPUT_SCHEMA_MISSING"))?,
        output_schema: capability
            .get("output_schema")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_OUTPUT_SCHEMA_MISSING"))?,
        idempotent: capability
            .get("idempotent")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_IDEMPOTENCY_MISSING"))?,
        created_at: Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    manifest.validate_all()?;
    Ok(manifest)
}

pub fn append_invocation_proposed(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    intent: &InvocationIntent,
) -> Result<()> {
    journal.append_event(
        JournalEventKind::InvocationProposed,
        Some(&run.id),
        Some(&session.id),
        Some(&intent.invocation_id.0),
        serde_json::json!({
            "invocation_id": intent.invocation_id.0,
            "operation": intent.operation,
            "idempotency_key": intent.idempotency_key,
        }),
    )?;
    Ok(())
}

pub fn append_invocation_approved(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    approved: &ApprovedInvocation,
) -> Result<()> {
    journal.append_event(
        JournalEventKind::InvocationApproved,
        Some(&run.id),
        Some(&session.id),
        Some(&approved.intent().invocation_id.0),
        serde_json::json!({
            "invocation_id": approved.intent().invocation_id.0,
            "operation": approved.intent().operation,
            "decision_id": approved.decision_id,
        }),
    )?;
    Ok(())
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("MISSING_{key}"))
}

fn string_set_matches(value: &Value, key: &str, expected: &[String]) -> Result<bool> {
    let values = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MISSING_{key}"))?;
    let actual = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("INVALID_{key}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let actual_set: std::collections::HashSet<_> = actual.iter().collect();
    let expected_set: std::collections::HashSet<_> = expected.iter().collect();
    Ok(actual.len() == expected.len() && actual_set == expected_set)
}
