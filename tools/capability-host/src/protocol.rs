//! Process-harness-v1 protocol types and external-harness-v1 response mapping.
//!
//! The Kernel sends an `external-harness-v1` request to the Capability Host.
//! The Capability Host transforms it into a `process-harness-v1` request on
//! the artifact's stdin, then maps the artifact's stdout response back to
//! the `external-harness-v1` response format.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Request body sent to the artifact's stdin.
#[derive(Debug, Serialize)]
pub struct ProcessRequest {
    pub protocol_version: String,
    pub operation_name: String,
    pub invocation_id: String,
    pub arguments: Value,
}

/// Successful artifact response (stdout JSON).
#[derive(Debug, Deserialize)]
pub(crate) struct ProcessSuccess {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<ProcessError>,
}

/// Structured error from the artifact.
#[derive(Debug, Deserialize)]
pub(crate) struct ProcessError {
    pub code: String,
    #[allow(dead_code)]
    pub message: Option<String>,
}

/// Parse the external-harness-v1 request body from the Kernel.
/// Returns the fields needed to execute the artifact.
pub struct HarnessRequest {
    #[allow(dead_code)]
    pub protocol_version: String,
    pub operation_name: String,
    #[allow(dead_code)]
    pub invocation_id: String,
    pub arguments: Value,
    #[allow(dead_code)]
    pub manifest_id: String,
    pub artifact_digest: String,
}

/// Parse and validate an incoming external-harness-v1 request.
pub fn parse_harness_request(body: &Value) -> Result<HarnessRequest, String> {
    let protocol_version = body
        .get("protocol_version")
        .and_then(Value::as_str)
        .unwrap_or("");
    if protocol_version != "external-harness-v1" {
        return Err(format!("unsupported_protocol"));
    }

    let manifest_id = body
        .get("manifest_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let artifact_digest = body
        .get("artifact_digest")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if manifest_id.is_empty() || artifact_digest.is_empty() {
        return Err("missing_manifest_identity".to_string());
    }

    let operation_name = body
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let invocation_id = body
        .get("invocation_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let arguments = body.get("arguments").cloned().unwrap_or(Value::Null);

    Ok(HarnessRequest {
        protocol_version: protocol_version.to_string(),
        operation_name,
        invocation_id,
        arguments,
        manifest_id,
        artifact_digest,
    })
}

/// Build a process-harness-v1 stdin payload for the artifact.
pub fn build_process_request(req: &HarnessRequest) -> ProcessRequest {
    ProcessRequest {
        protocol_version: "process-harness-v1".to_string(),
        operation_name: req.operation_name.clone(),
        invocation_id: req.invocation_id.clone(),
        arguments: req.arguments.clone(),
    }
}

/// Map the artifact's stdout response to an external-harness-v1 response body.
/// Returns `(ok_bool, response_body_json)`.
pub fn map_process_response(
    stdout: &str,
) -> (bool, serde_json::Value) {
    let parsed: ProcessSuccess = match serde_json::from_str(stdout) {
        Ok(p) => p,
        Err(_) => {
            return (
                false,
                serde_json::json!({
                    "protocol_version": "external-harness-v1",
                    "ok": false,
                    "error_code": "artifact_protocol_error",
                }),
            );
        }
    };

    if !parsed.ok {
        let code = parsed
            .error
            .as_ref()
            .map(|e| e.code.as_str())
            .unwrap_or("artifact_failed");
        return (
            false,
            serde_json::json!({
                "protocol_version": "external-harness-v1",
                "ok": false,
                "error_code": code,
            }),
        );
    }

    let result = parsed.result.unwrap_or(Value::Null);
    (
        true,
        serde_json::json!({
            "protocol_version": "external-harness-v1",
            "ok": true,
            "result": result,
        }),
    )
}
