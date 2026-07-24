mod hook_consumer;
mod invocable;
mod model;

use agent_core_kernel::domain::{DevelopmentRequest, TargetKind};
use serde_json::Value;
use std::path::Path;

#[derive(Debug)]
pub(super) struct GenerationError {
    code: &'static str,
}

impl GenerationError {
    pub(super) fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(super) fn code(&self) -> &'static str {
        self.code
    }
}

impl From<std::io::Error> for GenerationError {
    fn from(_: std::io::Error) -> Self {
        Self::new("CANDIDATE_GENERATION_FAILED")
    }
}

pub(super) fn generate(
    artifact_root: &Path,
    request: &DevelopmentRequest,
) -> Result<Value, GenerationError> {
    match request.target_kind {
        TargetKind::InvocableCapability => invocable::generate(artifact_root, request),
        TargetKind::HookConsumerService => hook_consumer::generate(artifact_root, request),
        _ => Err(GenerationError::new("GENERATOR_NOT_CONFIGURED_FOR_PROFILE")),
    }
}
