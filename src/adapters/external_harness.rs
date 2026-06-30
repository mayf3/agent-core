//! External Harness adapter — strict localhost HTTP transport for external
//! operations.  Only loopback addresses are allowed; the transport is
//! synchronous, single-shot, with bounded timeouts and response size.

use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 64 * 1024; // 64 KiB

/// Execute an external harness operation over strict localhost HTTP.
/// The adapter is stateless: all configuration comes from the manifest.
pub fn execute_external_harness(
    manifest: &HarnessManifest,
    invocation: &ApprovedInvocation,
) -> Result<Receipt> {
    let invocation_id = invocation.intent().invocation_id.clone();

    // Build request body.
    let request_body = serde_json::json!({
        "protocol_version": manifest.protocol_version,
        "invocation_id": invocation_id.0,
        "operation": manifest.operation_name,
        "arguments": invocation.intent().arguments,
    });
    let request_bytes = serde_json::to_vec(&request_body)?;

    // Resolve endpoint (loopback-only, validated at registration time).
    let addr = manifest
        .endpoint
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("endpoint resolution failed: {e}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("endpoint resolved to no addresses"))?;

    // Connect with timeout.
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| anyhow::anyhow!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| anyhow::anyhow!("set_read_timeout failed: {e}"))?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(|e| anyhow::anyhow!("set_write_timeout failed: {e}"))?;
    let mut stream = stream; // owned for IO

    // Send HTTP POST request.
    let request = format!(
        "POST /execute HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        addr,
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
                if raw.len() > MAX_RESPONSE_BYTES {
                    bail!("response exceeds 64 KiB limit");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                bail!("external harness request timed out");
            }
            Err(e) => {
                bail!("read failed: {e}");
            }
        }
    }

    // Parse HTTP response.
    let response_str =
        String::from_utf8(raw).map_err(|_| anyhow::anyhow!("non-UTF-8 response from harness"))?;

    // Extract body (after \r\n\r\n).
    let body = response_str
        .find("\r\n\r\n")
        .map(|idx| &response_str[idx + 4..])
        .ok_or_else(|| anyhow::anyhow!("missing HTTP header terminator in harness response"))?;

    let harness_response: Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("invalid JSON from harness: {e}"))?;

    // Verify protocol_version envelope field.
    let resp_protocol = harness_response
        .get("protocol_version")
        .and_then(Value::as_str)
        .unwrap_or("");
    if resp_protocol != manifest.protocol_version {
        bail!(
            "protocol version mismatch: got {resp_protocol:?}, expected {:?}",
            manifest.protocol_version
        );
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
        crate::registry::schema::validate_against_schema(&manifest.output_schema, result)
            .map_err(|e| anyhow::anyhow!("output schema violation: {e}"))?;

        Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Succeeded,
            output: result.clone(),
            external_ref: None,
            occurred_at: Utc::now(),
        })
    } else {
        let error_code = harness_response
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        Ok(Receipt {
            invocation_id,
            status: ReceiptStatus::Failed,
            output: serde_json::json!({"error": error_code}),
            external_ref: None,
            occurred_at: Utc::now(),
        })
    }
}
