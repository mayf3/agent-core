//! HTTP hook client — production implementation of `HookClient`.
//!
//! Uses `ureq` to send context.prepare.v0 requests to an External Harness
//! endpoint.  Enforces timeout, response size limits, and maps errors to
//! stable `error_code` values suitable for HookCallRecorded auditing.

use crate::hook::{
    ContextPrepareRequest, ContextPrepareResponse, HookClient, HookConfig, HookKind, HookLimits,
    HookResponseEnvelope,
};
use anyhow::{bail, Result};
use chrono::Utc;
use std::time::Duration;

/// A hook client that sends HTTP requests to a configured endpoint.
#[derive(Debug)]
pub struct HttpHookClient;

impl HttpHookClient {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HttpHookClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HookClient for HttpHookClient {
    fn call_context_prepare(
        &self,
        request: &ContextPrepareRequest,
        config: &HookConfig,
    ) -> Result<ContextPrepareResponse> {
        let url = config.endpoint.url.trim();
        if url.is_empty() {
            bail!("endpoint_missing");
        }

        let limits: HookLimits = config.into();

        // Build request envelope as a JSON value (compatible with send_json).
        let envelope = serde_json::json!({
            "hook": "context.prepare.v0",
            "request_id": format!("ctx_{}", uuid::Uuid::new_v4().simple()),
            "timestamp": Utc::now().to_rfc3339(),
            "payload": request,
        });

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(limits.timeout_ms)))
            .build()
            .new_agent();

        let response = agent
            .post(url)
            .header("content-type", "application/json")
            .send_json(envelope);

        match response {
            Ok(resp) => {
                // Read response body with size limit.
                let max_bytes = limits.max_response_bytes as usize;
                let body_str = resp.into_body().read_to_string()?;
                if body_str.len() > max_bytes {
                    bail!("response_too_large");
                }

                // Parse response envelope.
                let resp_envelope: HookResponseEnvelope =
                    serde_json::from_str(&body_str).map_err(|_| anyhow::anyhow!("invalid_json"))?;

                if resp_envelope.hook != HookKind::ContextPrepareV0 {
                    bail!("unsupported_hook_response");
                }

                let prepare_resp: ContextPrepareResponse =
                    serde_json::from_value(resp_envelope.payload)
                        .map_err(|_| anyhow::anyhow!("invalid_json"))?;

                Ok(prepare_resp)
            }
            Err(ureq::Error::StatusCode(code)) => {
                let label = if (400..=499).contains(&code) {
                    "http_status_4xx"
                } else if (500..=599).contains(&code) {
                    "http_status_5xx"
                } else {
                    "http_status_unknown"
                };
                bail!("{label}:{code}");
            }
            Err(ureq::Error::Timeout(_)) => {
                bail!("http_timeout");
            }
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("connection refused") || msg.contains("dns") {
                    bail!("http_connect_error");
                }
                bail!("http_transport_error");
            }
        }
    }
}
