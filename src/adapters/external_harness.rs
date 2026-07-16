//! External Harness adapter — strict localhost HTTP transport for external
//! operations.  Only loopback addresses are allowed; the transport is
//! synchronous, single-shot, with bounded timeouts and response size.

use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Transport configuration for external harness execution.
/// Defaults are safe for production; tests can reduce timeouts.
pub struct ExternalHarnessTransportConfig {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub max_response_bytes: usize,
    /// Capability Host execution bearer. Generic legacy harnesses may ignore
    /// the header; the hardened Capability Host rejects a missing value.
    pub bearer_token: Option<String>,
}

impl Default for ExternalHarnessTransportConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            read_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
            bearer_token: std::env::var("AGENT_CORE_CAPABILITY_HOST_EXECUTION_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty()),
        }
    }
}

pub fn execute_external_harness(
    manifest: &HarnessManifest,
    invocation: &ApprovedInvocation,
) -> Result<Receipt> {
    execute_external_harness_with_config(
        manifest,
        invocation,
        &ExternalHarnessTransportConfig::default(),
        "",
    )
}

/// Execute an external harness operation with configurable transport.
/// The `registry_snapshot_id` is the Run's pinned snapshot (empty string if
/// unavailable) — it is forwarded to the harness so the Capability Host can
/// audit which snapshot selected this operation.
pub fn execute_external_harness_with_config(
    manifest: &HarnessManifest,
    invocation: &ApprovedInvocation,
    config: &ExternalHarnessTransportConfig,
    registry_snapshot_id: &str,
) -> Result<Receipt> {
    let invocation_id = invocation.intent().invocation_id.clone();

    // Strip internal-only fields from arguments before building request body.
    // session_id was injected for policy validation and must not reach the harness.
    let mut clean_args = invocation.intent().arguments.clone();
    if let Some(obj) = clean_args.as_object_mut() {
        obj.remove("session_id");
    }

    // Build request body WITH authoritative manifest identity.
    // manifest_id and artifact_digest come from the manifest (never from LLM
    // arguments). registry_snapshot_id comes from the Run's pinned snapshot.
    // Old external harnesses that ignore unknown fields continue to work.
    let request_body = serde_json::json!({
        "protocol_version": manifest.protocol_version,
        "invocation_id": invocation_id.0,
        "operation": manifest.operation_name,
        "arguments": clean_args,
        "manifest_id": manifest.manifest_id,
        "artifact_digest": manifest.artifact_digest,
        "registry_snapshot_id": registry_snapshot_id,
    });
    let request_bytes = serde_json::to_vec(&request_body)?;

    // Parse endpoint.
    let parsed = manifest.parse_endpoint()?;
    let addr_str = format!("{}:{}", parsed.host, parsed.port);

    // Resolve and verify all addresses are loopback.
    let addrs: Vec<_> = addr_str
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("endpoint resolution failed: {e}"))?
        .collect();
    if addrs.is_empty() {
        bail!("endpoint resolved to no addresses");
    }
    for addr in &addrs {
        if !addr.ip().is_loopback() {
            bail!("resolved address {addr} is not a loopback address");
        }
    }

    // Connect with timeout.
    let stream = TcpStream::connect_timeout(&addrs[0], config.connect_timeout)
        .map_err(|e| anyhow::anyhow!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(config.read_timeout))
        .map_err(|e| anyhow::anyhow!("set_read_timeout failed: {e}"))?;
    stream
        .set_write_timeout(Some(config.write_timeout))
        .map_err(|e| anyhow::anyhow!("set_write_timeout failed: {e}"))?;
    let mut stream = stream;

    // Send HTTP POST request using the manifest's path.
    let authorization = config
        .bearer_token
        .as_deref()
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {} HTTP/1.1\r\n\
         Host: {}\r\n\
         {}\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        parsed.path,
        addr_str,
        authorization,
        request_bytes.len(),
        String::from_utf8_lossy(&request_bytes),
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| anyhow::anyhow!("write failed: {e}"))?;

    // Read response with size limit.
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() > config.max_response_bytes {
                    bail!("response exceeds {} byte limit", config.max_response_bytes);
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                bail!("external harness request timed out");
            }
            Err(e) => {
                bail!("read failed: {e}");
            }
        }
    }

    let response_str =
        String::from_utf8(raw).map_err(|_| anyhow::anyhow!("non-UTF-8 response from harness"))?;

    // Parse HTTP status line with strict numeric parsing.
    let status_line = response_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = status_line.split_whitespace().collect();
    let status_code: u16 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    if status_code == 0 {
        // Illegal status line → malformed_response.
        return Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Failed,
            output: serde_json::json!({"error_category": "malformed_response"}),
            external_ref: None,
            occurred_at: Utc::now(),
        });
    }
    if !(200..300).contains(&status_code) {
        return Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Failed,
            output: serde_json::json!({"error_category": "http_error", "http_code": status_code}),
            external_ref: None,
            occurred_at: Utc::now(),
        });
    }

    // Extract body (after \r\n\r\n).
    let body = response_str
        .find("\r\n\r\n")
        .map(|idx| &response_str[idx + 4..])
        .ok_or_else(|| anyhow::anyhow!("missing HTTP header terminator in harness response"))?;

    // Empty body with 2xx is malformed.
    if body.is_empty() {
        return Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Failed,
            output: serde_json::json!({"error_category": "malformed_response"}),
            external_ref: None,
            occurred_at: Utc::now(),
        });
    }

    let harness_response: Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("invalid JSON from harness: {e}"))?;

    // Verify protocol_version envelope field.
    let resp_protocol = harness_response
        .get("protocol_version")
        .and_then(Value::as_str)
        .unwrap_or("");
    if resp_protocol != manifest.protocol_version {
        return Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Failed,
            output: serde_json::json!({"error_category": "protocol_mismatch"}),
            external_ref: None,
            occurred_at: Utc::now(),
        });
    }

    // Check ok status.
    let ok = harness_response
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if ok {
        let result = harness_response
            .get("result")
            .ok_or_else(|| anyhow::anyhow!("harness returned ok=true but no result"))?;

        // Validate against output schema.
        if crate::registry::schema::validate_against_schema(&manifest.output_schema, result)
            .is_err()
        {
            return Ok(Receipt {
                invocation_id,
                status: ReceiptStatus::Failed,
                output: serde_json::json!({"error_category": "output_schema_violation"}),
                external_ref: None,
                occurred_at: Utc::now(),
            });
        }

        let external_ref = harness_response
            .get("capability_host_execution_id")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 160
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
            })
            .map(str::to_string);
        if manifest.operation_name == "external.calculator"
            && config.bearer_token.is_some()
            && external_ref.is_none()
        {
            return Ok(Receipt {
                invocation_id,
                status: ReceiptStatus::Failed,
                output: serde_json::json!({"error_category": "missing_execution_identity"}),
                external_ref: None,
                occurred_at: Utc::now(),
            });
        }
        Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Succeeded,
            output: result.clone(),
            external_ref,
            occurred_at: Utc::now(),
        })
    } else {
        // Harness returned ok=false. Map known harness error codes to
        // stable error categories so the kernel's error routing preserves
        // actionable diagnostics instead of lumping everything into
        // "harness_failed". Unknown codes fall back to the generic category.
        let raw_code = harness_response
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("");
        let bounded: String = raw_code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(64)
            .collect();
        let category = match bounded.as_str() {
            "GENERATOR_MODEL_NOT_CONFIGURED"
            | "GENERATOR_NOT_CONFIGURED_FOR_PROFILE"
            | "UNKNOWN_COMPONENT_PROFILE"
            | "INVALID_DEVELOPMENT_REQUEST"
            | "UNSUPPORTED_TARGET_KIND" => "generator_config",
            "HARNESS_UNAVAILABLE" | "CONNECTION_REFUSED" | "TIMEOUT" => "harness_unavailable",
            "CANDIDATE_REJECTED" | "CANDIDATE_GENERATION_FAILED" => "candidate_failed",
            "HCR_INFRASTRUCTURE_FAILURE" | "SETTLEMENT_FAILED" => "hcr_infrastructure",
            _ => "harness_failed",
        };
        let mut output = serde_json::json!({"error_category": category});
        if !bounded.is_empty() {
            output["harness_error_code"] = json!(bounded);
        }
        Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Failed,
            output,
            external_ref: None,
            occurred_at: Utc::now(),
        })
    }
}
