//! Delivery manifest dispatcher — routes to the correct builder based
//! on the candidate's `target_kind`.
//!
//! # Architecture
//!
//! The acceptance pipeline calls `build_delivery_manifest()` after all
//! five gates have passed.  This function inspects `target_kind` from
//! the immutable candidate component manifest and dispatches to the
//! appropriate builder:
//!
//! | target_kind            | builder                | output type       |
//! |------------------------|------------------------|-------------------|
//! | `HookConsumerService`  | `service_manifest`     | `ServiceManifest` |
//! | `InvocableCapability`  | `invocable_manifest`   | `HarnessManifest` |
//!
//! Both builders return serialisable bytes and a deterministic ref.
//! The caller stores them in the shared ContentStore and returns
//! content‑addressed ref/digest — the Kernel never parses the bytes.

use agent_core_kernel::domain::DevelopmentRequest;
use anyhow::{anyhow, Result};
use serde_json::Value;

use super::invocable_manifest::build_invocable_manifest;
use super::service_manifest::build_service_manifest;

/// Build the final delivery manifest bytes and ref from the accepted
/// candidate component manifest.
///
/// `development_request` is required for `InvocableCapability` (identity
/// validation) and may be `None` for `HookConsumerService` (which only
/// uses the candidate manifest + artifact digest).
pub fn build_delivery_manifest(
    component: &Value,
    artifact_digest: &str,
    development_request: Option<&DevelopmentRequest>,
) -> Result<(String, Vec<u8>)> {
    let target_kind = component
        .get("target_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_TARGET_KIND"))?;

    match target_kind {
        "HookConsumerService" => {
            let manifest = build_service_manifest(component, artifact_digest)?;
            let bytes = serde_json::to_vec(&manifest)?;
            Ok((manifest.manifest_id.clone(), bytes))
        }
        "InvocableCapability" => {
            let request = development_request
                .ok_or_else(|| anyhow!("INVOCABLE_MANIFEST_REQUIRES_DEVELOPMENT_REQUEST"))?;
            let manifest = build_invocable_manifest(component, artifact_digest, request)?;
            let bytes = serde_json::to_vec(&manifest)?;
            Ok((manifest.manifest_id.clone(), bytes))
        }
        other => Err(anyhow!("UNEXPECTED_TARGET_KIND: {other}")),
    }
}
