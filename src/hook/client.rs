//! Transport-neutral client boundary for hook calls.

use crate::hook::{
    AuthenticatedContextHookResponse, AuthenticatedRunBudgetResponse, ContextHookRequest,
    ContextHookResponse, HookConfig, RunBudgetHookRequest, RunBudgetHookResponse,
};
use anyhow::{bail, Result};

pub trait HookClient: std::fmt::Debug {
    fn call_context(
        &self,
        request: &ContextHookRequest,
        config: &HookConfig,
    ) -> Result<AuthenticatedContextHookResponse>;

    fn call_budget(
        &self,
        _request: &RunBudgetHookRequest,
        _config: &HookConfig,
    ) -> Result<AuthenticatedRunBudgetResponse> {
        // Default: budget calls are not supported by this client. The Runtime
        // only invokes call_budget when a budget_hook is explicitly configured,
        // so this default is a safety net for test/stub implementations.
        bail!("budget_hook_not_supported_by_client")
    }
}

#[derive(Debug)]
pub struct FakeHookClient {
    pub response: Option<ContextHookResponse>,
    pub budget_response: Option<RunBudgetHookResponse>,
    pub error_code: Option<String>,
}

impl FakeHookClient {
    pub fn passthrough() -> Self {
        Self {
            response: None,
            budget_response: None,
            error_code: None,
        }
    }

    pub fn with_response(response: ContextHookResponse) -> Self {
        Self {
            response: Some(response),
            budget_response: None,
            error_code: None,
        }
    }

    pub fn with_budget_response(response: RunBudgetHookResponse) -> Self {
        Self {
            response: None,
            budget_response: Some(response),
            error_code: None,
        }
    }

    pub fn with_error(error_code: &str) -> Self {
        Self {
            response: None,
            budget_response: None,
            error_code: Some(error_code.into()),
        }
    }
}

impl HookClient for FakeHookClient {
    fn call_context(
        &self,
        request: &ContextHookRequest,
        config: &HookConfig,
    ) -> Result<AuthenticatedContextHookResponse> {
        if let Some(error_code) = &self.error_code {
            bail!("{error_code}");
        }
        let response = self
            .response
            .clone()
            .unwrap_or_else(|| ContextHookResponse {
                run_id: request.candidate.run_id.clone(),
                session_id: request.candidate.session_id.clone(),
                scope_digest: request.candidate.scope_digest.clone(),
                candidate_digest: request.candidate.artifact.digest.clone(),
                immutable_refs: request.candidate.immutable_refs.clone(),
                immutable_refs_digest: request.candidate.immutable_refs_digest.clone(),
                artifacts: vec![request.candidate.artifact.clone()],
            });
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
        if let Some(error_code) = &self.error_code {
            bail!("{error_code}");
        }
        let response = self
            .budget_response
            .clone()
            .unwrap_or_else(|| RunBudgetHookResponse {
                request_id: request.request_id.clone(),
                run_id: request.run_id.clone(),
                decision: crate::hook::RunBudgetDecision {
                    max_tool_rounds: 12,
                    max_wall_time_ms: 300_000,
                    exhaustion_action: crate::hook::ExhaustionAction::Yield,
                },
            });
        Ok(AuthenticatedRunBudgetResponse {
            provider_id: config.provider_id.clone(),
            request_id: request.request_id.clone(),
            response,
        })
    }
}
