//! Transport-neutral client boundary for the unified pre-model Context Hook.

use crate::hook::{
    AuthenticatedContextHookResponse, ContextHookRequest, ContextHookResponse, HookConfig,
};
use anyhow::{bail, Result};

pub trait HookClient: std::fmt::Debug {
    fn call_context(
        &self,
        request: &ContextHookRequest,
        config: &HookConfig,
    ) -> Result<AuthenticatedContextHookResponse>;
}

#[derive(Debug)]
pub struct FakeHookClient {
    pub response: Option<ContextHookResponse>,
    pub error_code: Option<String>,
}

impl FakeHookClient {
    pub fn passthrough() -> Self {
        Self {
            response: None,
            error_code: None,
        }
    }

    pub fn with_response(response: ContextHookResponse) -> Self {
        Self {
            response: Some(response),
            error_code: None,
        }
    }

    pub fn with_error(error_code: &str) -> Self {
        Self {
            response: None,
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
}
