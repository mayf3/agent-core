//! Delivery manifest dispatcher — routes to the correct builder based
//! on the verified DevelopmentRequest's `target_kind`.
//!
//! # Architecture
//!
//! The acceptance pipeline calls `build_delivery_manifest()` after all
//! five gates have passed.  Builder selection is driven exclusively by
//! `development_request.target_kind` (from the verified HCR requirement).
//! The candidate manifest's `kind` field is ONLY used for cross-validation:
//! it must match the development request's target_kind, otherwise the
//! acceptance fails closed (no candidate-controlled dispatch).
//!
//! | request.target_kind    | builder                | output type       |
//! |------------------------|------------------------|-------------------|
//! | `HookConsumerService`  | `service_manifest`     | `ServiceManifest` |
//! | `InvocableCapability`  | `invocable_manifest`   | `HarnessManifest` |
//!
//! Both builders return serialisable bytes and a deterministic ref.
//! The caller stores them in the shared ContentStore and returns
//! content‑addressed ref/digest — the Kernel never parses the bytes.

use agent_core_kernel::domain::{DevelopmentRequest, TargetKind};
use anyhow::{anyhow, Result};
use serde_json::Value;

use super::invocable_manifest::build_invocable_manifest;
use super::service_manifest::build_service_manifest;

/// Build the final delivery manifest bytes and ref from the accepted
/// candidate component manifest.
///
/// `development_request` is REQUIRED and its `target_kind` selects the
/// builder. The candidate manifest's `kind` field is validated against
/// the request: a mismatch is rejected.
pub fn build_delivery_manifest(
    component: &Value,
    artifact_digest: &str,
    development_request: Option<&DevelopmentRequest>,
) -> Result<(String, Vec<u8>)> {
    let request = development_request
        .ok_or_else(|| anyhow!("DEVELOPMENT_REQUEST_REQUIRED_FOR_MANIFEST_BUILD"))?;

    // Validate candidate.kind matches request.target_kind
    let candidate_kind = component
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("CANDIDATE_MISSING_KIND"))?;

    let expected_kind = match request.target_kind {
        TargetKind::InvocableCapability => "invocable_capability",
        TargetKind::HookConsumerService => "hook_consumer_service",
        _ => {
            return Err(anyhow!(
                "UNSUPPORTED_TARGET_KIND: {:?}",
                request.target_kind
            ))
        }
    };

    if candidate_kind != expected_kind {
        return Err(anyhow!(
            "CANDIDATE_KIND_MISMATCH: request.target_kind={:?} candidate.kind={}",
            request.target_kind,
            candidate_kind
        ));
    }

    match request.target_kind {
        TargetKind::HookConsumerService => {
            let manifest = build_service_manifest(component, artifact_digest)?;
            let bytes = serde_json::to_vec(&manifest)?;
            Ok((manifest.manifest_id.clone(), bytes))
        }
        TargetKind::InvocableCapability => {
            let manifest = build_invocable_manifest(component, artifact_digest, request)?;
            let bytes = serde_json::to_vec(&manifest)?;
            Ok((manifest.manifest_id.clone(), bytes))
        }
        _ => Err(anyhow!("UNEXPECTED_TARGET_KIND: {:?}", request.target_kind)),
    }
}
