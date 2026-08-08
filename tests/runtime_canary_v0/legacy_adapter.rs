//! CANARY / LEGACY COMPATIBILITY ONLY.
//!
//! Mechanical translation between the canary's narrow invocation port and
//! the shared external-harness execution mechanism. Everything here is
//! test compatibility debt — the canary deletes together with this module.
//! This is NOT the V2.1 Kernel boundary.
//!
//! Second cut: the legacy governance simulation is gone. The adapter now
//! only resolves the test capability reference `C17` to an inline test
//! binding and hands it to the shared execution mechanism.

use agent_core_kernel::adapters::external_harness::{
    execute_external_harness_binding_for_tests, ExternalHarnessTransportConfig,
};
use agent_core_kernel::domain::{InvocationId, ReceiptStatus};
use agent_core_kernel::harness::manifest::HarnessManifest;
use crate::canary_runtime::{self, InvocationPort, InvocationResult, InvocationStatus};
use serde_json::{json, Value};
use std::time::Duration;

/// TEST COMPATIBILITY DEBT: the product operation name the test binding
/// maps C17 to (kept only until the canary has a real capability
/// directory).
const BOUND_OPERATION: &str = "external.coding_workspace_exec";

pub struct CanaryBindingAdapter {
    /// TEST COMPATIBILITY DEBT: inline test binding (not registered
    /// anywhere; the canary resolves it directly).
    binding: HarnessManifest,
    transport: ExternalHarnessTransportConfig,
}

impl CanaryBindingAdapter {
    pub fn new(provider_endpoint: String, provider_token: String) -> Self {
        let binding = HarnessManifest {
            manifest_id: String::new(), // TEST COMPATIBILITY DEBT: not registered
            harness_id: "canary-v0".into(),
            artifact_digest:
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            protocol_version: "external-harness-v1".into(),
            endpoint: provider_endpoint,
            operation_name: BOUND_OPERATION.into(),
            description: "canary test binding (deletable)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": {"type": "string", "minLength": 1},
                    "command": {"type": "string", "minLength": 1},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 120}
                },
                "required": ["workspace_id", "command"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": {"type": "string"},
                    "exit_code": {"type": "integer"},
                    "stdout": {"type": "string"},
                    "stderr": {"type": "string"},
                    "timed_out": {"type": "boolean"}
                },
                "required": ["workspace_id", "exit_code", "stdout", "stderr", "timed_out"]
            }),
            idempotent: false,
            created_at: chrono::Utc::now(),
        };
        let transport = ExternalHarnessTransportConfig {
            connect_timeout: Duration::from_secs(3),
            read_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
            bearer_token: Some(provider_token),
        };
        Self { binding, transport }
    }
}

impl InvocationPort for CanaryBindingAdapter {
    fn submit(
        &self,
        invocation_id: &str,
        capability_ref: &str,
        arguments: Value,
    ) -> Result<InvocationResult, String> {
        if capability_ref != canary_runtime::C17 {
            return Err(format!("unknown capability reference: {capability_ref}"));
        }
        // TEST COMPATIBILITY DEBT: snapshot id placeholder ("" = none; the
        // current provider protocol tolerates an empty audit field).
        let receipt = execute_external_harness_binding_for_tests(
            &self.binding,
            &InvocationId(invocation_id.into()),
            &arguments,
            &self.transport,
            "",
        )
        .map_err(|e| format!("provider dispatch failed: {e}"))?;
        let status = match receipt.status {
            ReceiptStatus::Succeeded => InvocationStatus::Succeeded,
            ReceiptStatus::Failed => InvocationStatus::Failed,
            ReceiptStatus::Unknown => InvocationStatus::Unknown,
        };
        Ok(InvocationResult {
            invocation_id: invocation_id.into(),
            status,
            output: receipt.output,
        })
    }
}
