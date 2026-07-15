use super::{generate_module, repair_module, GenerationError, ModelConfig};
use agent_core_kernel::domain::DevelopmentRequest;

pub(in crate::self_evolution::generator) fn generate_module_with_retry(
    config: &ModelConfig,
    request: &DevelopmentRequest,
) -> Result<(String, usize), GenerationError> {
    retry_model_output(3, || generate_module(config, request))
}

pub(in crate::self_evolution::generator) fn repair_module_with_retry(
    config: &ModelConfig,
    request: &DevelopmentRequest,
    previous_source: &str,
    compiler_diagnostics: &str,
    max_attempts: usize,
) -> Result<(String, usize), GenerationError> {
    retry_model_output(max_attempts, || {
        repair_module(config, request, previous_source, compiler_diagnostics)
    })
}

pub(super) fn retry_model_output<F>(
    max_attempts: usize,
    mut operation: F,
) -> Result<(String, usize), GenerationError>
where
    F: FnMut() -> Result<String, GenerationError>,
{
    if max_attempts == 0 {
        return Err(GenerationError::new("GENERATOR_COMPILE_REPAIR_EXHAUSTED"));
    }
    for attempt in 0..max_attempts {
        match operation() {
            Ok(source) => return Ok((source, attempt + 1)),
            Err(error)
                if attempt + 1 < max_attempts && retryable_model_output_error(error.code()) => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("the final model attempt always returns")
}

pub(super) fn retryable_model_output_error(code: &str) -> bool {
    matches!(
        code,
        "GENERATOR_MODEL_UNAVAILABLE" | "GENERATOR_MODEL_RESPONSE_INVALID"
    ) || code.starts_with("GENERATOR_MODEL_OUTPUT_")
}
