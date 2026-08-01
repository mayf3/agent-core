//! Authenticated HTTP transport for hook calls.

use crate::hook::{
    verify_budget_provider_proof, verify_provider_proof, AuthenticatedContextHookResponse,
    AuthenticatedRunBudgetResponse, ContextHookRequest, ContextHookResponse, HookClient,
    HookConfig, HookKind, HookLimits, HookResponseEnvelope, RunBudgetHookRequest,
    RunBudgetHookResponse,
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

/// Shared HTTP POST logic for both context and budget hooks. Returns the raw
/// response body string and the `x-agent-core-provider-proof` header.
fn post_hook(
    hook_name: &str,
    request_id: &str,
    payload: &serde_json::Value,
    config: &HookConfig,
    limits: &HookLimits,
) -> Result<(String, String)> {
    let envelope = serde_json::json!({
        "hook": hook_name,
        "request_id": request_id,
        "timestamp": Utc::now().to_rfc3339(),
        "payload": payload,
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
            Ok((body, proof))
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

impl HookClient for HttpHookClient {
    fn call_context(
        &self,
        request: &ContextHookRequest,
        config: &HookConfig,
    ) -> Result<AuthenticatedContextHookResponse> {
        config.validate()?;
        let limits: HookLimits = config.into();
        let (body, proof) = post_hook(
            "context.prepare.v0",
            &request.request_id,
            &serde_json::to_value(request)?,
            config,
            &limits,
        )?;
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

    fn call_budget(
        &self,
        request: &RunBudgetHookRequest,
        config: &HookConfig,
    ) -> Result<AuthenticatedRunBudgetResponse> {
        config.validate()?;
        let limits: HookLimits = config.into();
        let (body, proof) = post_hook(
            "run.budget.resolve.v0",
            &request.request_id,
            &serde_json::to_value(request)?,
            config,
            &limits,
        )?;
        let envelope: HookResponseEnvelope =
            serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("invalid_json"))?;
        if envelope.hook != HookKind::RunBudgetResolveV0 {
            bail!("unsupported_hook_response");
        }
        if envelope.request_id != request.request_id {
            bail!("hook_request_id_mismatch");
        }
        let response: RunBudgetHookResponse = serde_json::from_value(envelope.payload)
            .map_err(|_| anyhow::anyhow!("invalid_budget_response"))?;
        response.validate_against(request)?;
        verify_budget_provider_proof(
            &config.shared_secret,
            &response.authentication_message(&config.provider_id),
            &proof,
        )?;
        Ok(AuthenticatedRunBudgetResponse {
            provider_id: config.provider_id.clone(),
            request_id: request.request_id.clone(),
            response,
        })
    }
}
