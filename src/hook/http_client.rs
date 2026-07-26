//! Authenticated HTTP transport for the unified pre-model Context Hook.

use crate::hook::{
    verify_provider_proof, AuthenticatedContextHookResponse, ContextHookRequest,
    ContextHookResponse, HookClient, HookConfig, HookKind, HookLimits, HookResponseEnvelope,
};
use anyhow::{bail, Result};
use chrono::Utc;
use std::time::Duration;

#[derive(Debug, Default)]
pub struct HttpHookClient;

impl HttpHookClient {
    pub fn new() -> Self {
        Self
    }
}

impl HookClient for HttpHookClient {
    fn call_context(
        &self,
        request: &ContextHookRequest,
        config: &HookConfig,
    ) -> Result<AuthenticatedContextHookResponse> {
        config.validate()?;
        let limits: HookLimits = config.into();
        let envelope = serde_json::json!({
            "hook": "context.prepare.v0",
            "request_id": request.request_id,
            "timestamp": Utc::now().to_rfc3339(),
            "payload": request,
        });
        if serde_json::to_vec(&envelope)?.len() > limits.max_request_bytes as usize {
            bail!("request_too_large");
        }

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(limits.timeout_ms)))
            .build()
            .new_agent();
        let response = agent
            .post(config.endpoint.url.trim())
            .header("authorization", &format!("Bearer {}", config.shared_secret))
            .header("content-type", "application/json")
            .send_json(envelope);

        match response {
            Ok(response) => {
                let proof = response
                    .headers()
                    .get("x-agent-core-provider-proof")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("provider_proof_missing"))?;
                let body = response
                    .into_body()
                    .with_config()
                    .limit(limits.max_response_bytes)
                    .read_to_string()
                    .map_err(|_| anyhow::anyhow!("response_too_large"))?;
                let envelope: HookResponseEnvelope =
                    serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("invalid_json"))?;
                if envelope.hook != HookKind::ContextPrepareV0 {
                    bail!("unsupported_hook_response");
                }
                if envelope.request_id != request.request_id {
                    bail!("hook_request_id_mismatch");
                }
                let response: ContextHookResponse = serde_json::from_value(envelope.payload)
                    .map_err(|_| anyhow::anyhow!("invalid_context_artifact_response"))?;
                response.validate_against(request)?;
                verify_provider_proof(
                    &config.shared_secret,
                    &response.authentication_message(&config.provider_id, &request.request_id),
                    &proof,
                )?;
                Ok(AuthenticatedContextHookResponse {
                    provider_id: config.provider_id.clone(),
                    request_id: request.request_id.clone(),
                    response,
                })
            }
            Err(ureq::Error::StatusCode(code)) => {
                let category = if (400..=499).contains(&code) {
                    "http_status_4xx"
                } else if (500..=599).contains(&code) {
                    "http_status_5xx"
                } else {
                    "http_status_unknown"
                };
                bail!("{category}:{code}");
            }
            Err(ureq::Error::Timeout(_)) => bail!("http_timeout"),
            Err(error) => {
                let message = error.to_string().to_lowercase();
                if message.contains("connection refused") || message.contains("dns") {
                    bail!("http_connect_error");
                }
                bail!("http_transport_error");
            }
        }
    }
}
