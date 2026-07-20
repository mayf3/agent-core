//! Delivery manifest construction from the immutable candidate.
//!
//! The accepted candidate `manifest.json` is never modified on disk.
//! Instead we work on an in‑memory clone: read the original, allocate
//! the correct version, then construct the final `ServiceManifest`.
//! The resulting delivery manifest is stored in the shared ContentStore
//! and bound by the receipt's `opaque_payload_digest`.

use agent_core_kernel::domain::service_manifest::{
    ListenPolicy, RollbackPolicy, SERVICE_MANIFEST_SCHEMA, ServiceHealthcheck, ServiceManifest,
    UpgradePolicy,
};
use anyhow::{anyhow, Result};
use serde_json::Value;

/// Construct the final `ServiceManifest` for a HookConsumerService
/// component from the immutable candidate artifact manifest and the
/// verified artifact digest.
///
/// The version field is already resolved by the time this function
/// is called — it either comes from the generator (initial version)
/// or was overridden via `super::version_allocation::allocate_next_version`.
pub fn build_service_manifest(
    component: &Value,
    artifact_digest: &str,
) -> Result<ServiceManifest> {
    let target_kind = component
        .get("target_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_TARGET_KIND"))?;
    if target_kind != "HookConsumerService" {
        return Err(anyhow!("UNEXPECTED_TARGET_KIND: {target_kind}"));
    }
    let _schema_version = component
        .get("schema_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_SCHEMA_VERSION"))?;
    let component_id = component
        .get("component_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_COMPONENT_ID"))?;

    let service = component
        .get("service")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("MISSING_SERVICE_OBJECT"))?;

    let version = service
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_SERVICE_VERSION"))?;

    let healthcheck_path = service
        .get("healthcheck_path")
        .and_then(|v| v.as_str())
        .unwrap_or("/health");

    let required_contracts: Vec<String> = component
        .get("required_contracts")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .ok_or_else(|| anyhow!("MISSING_REQUIRED_CONTRACTS"))?;

    let requested_permissions: Vec<String> = component
        .get("requested_permissions")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .ok_or_else(|| anyhow!("MISSING_REQUESTED_PERMISSIONS"))?;

    let mut manifest = ServiceManifest {
        schema_version: SERVICE_MANIFEST_SCHEMA.into(),
        manifest_id: String::new(),
        component_id: component_id.into(),
        kind: agent_core_kernel::domain::TargetKind::HookConsumerService,
        artifact_digest: artifact_digest.into(),
        entrypoint: "artifact".into(),
        runtime_profile: "managed-service-v0".into(),
        version: version.into(),
        required_contracts,
        requested_permissions,
        listen_policy: ListenPolicy {
            host: "127.0.0.1".into(),
            port: 0,
            exposure: "loopback".into(),
        },
        healthcheck: ServiceHealthcheck {
            method: "GET".into(),
            path: healthcheck_path.into(),
            timeout_ms: 10_000,
        },
        state_path: "state".into(),
        upgrade_policy: UpgradePolicy {
            strategy: "replace_after_ready".into(),
            require_healthy_before_switch: true,
        },
        rollback_policy: RollbackPolicy {
            retain_previous_versions: 2,
            automatic_on_health_failure: true,
        },
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    manifest
        .validate()
        .map_err(|e| anyhow!("MANIFEST_VALIDATION: {e}"))?;
    Ok(manifest)
}
