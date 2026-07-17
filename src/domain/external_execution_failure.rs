//! Kernel-understood external execution failure classification.
//!
//! These are generic failure classes that the Kernel understands without
//! needing to know about Acceptance Kits, Bundles, or any business-specific
//! concepts. The external Harness returns a `failure_class` in its response;
//! the Kernel maps it to this enum and does not branch on the `detail_code`.
//!
//! The Harness's `detail_code` (e.g. "ACCEPTANCE_KIT_SELECTION_REQUIRED")
//! is passed through as an opaque diagnostic string for logging only.

use serde::{Deserialize, Serialize};

/// Generic classification for external execution failures.
///
/// The Kernel uses this to determine the appropriate user-facing message
/// and safe error handling, without needing to understand the specific
/// external failure domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalExecutionFailureClass {
    /// The external system requires additional input to proceed.
    ExternalInputRequired,
    /// A required external reference could not be found.
    ExternalReferenceNotFound,
    /// A binding mismatch occurred (e.g. subject digest changed).
    ExternalBindingMismatch,
    /// External verification (acceptance tests) failed.
    ExternalVerificationFailed,
    /// External build/compilation failed.
    ExternalBuildFailed,
    /// External output was rejected (e.g. unsafe content).
    ExternalOutputRejected,
    /// External service is unavailable.
    ExternalUnavailable,
    /// External configuration is missing or invalid.
    ExternalConfigurationMissing,
    /// External infrastructure failure.
    ExternalInfrastructureFailure,
}

impl ExternalExecutionFailureClass {
    /// Return the stable string representation for protocol serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExternalInputRequired => "external_input_required",
            Self::ExternalReferenceNotFound => "external_reference_not_found",
            Self::ExternalBindingMismatch => "external_binding_mismatch",
            Self::ExternalVerificationFailed => "external_verification_failed",
            Self::ExternalBuildFailed => "external_build_failed",
            Self::ExternalOutputRejected => "external_output_rejected",
            Self::ExternalUnavailable => "external_unavailable",
            Self::ExternalConfigurationMissing => "external_configuration_missing",
            Self::ExternalInfrastructureFailure => "external_infrastructure_failure",
        }
    }

    /// Parse a failure_class string from the external Harness protocol.
    ///
    /// This is the only place in the Kernel where Harness error strings
    /// are mapped to failure classes. The `detail_code` from the Harness
    /// is passed through as an opaque string and is NOT parsed here.
    pub fn from_harness_code(code: &str) -> Self {
        match code {
            "ACCEPTANCE_KIT_SELECTION_REQUIRED" | "external_input_required" => {
                Self::ExternalInputRequired
            }
            "ACCEPTANCE_KIT_NOT_FOUND" | "external_reference_not_found" => {
                Self::ExternalReferenceNotFound
            }
            "ACCEPTANCE_KIT_DIGEST_MISMATCH" | "external_binding_mismatch" => {
                Self::ExternalBindingMismatch
            }
            "GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED"
            | "CROSS_KIT_CONTAMINATION_FAILED"
            | "COMBINED_CONTRACT_PROBE_FAILED"
            | "external_verification_failed" => Self::ExternalVerificationFailed,
            "GENERATOR_COMPILE_REPAIR_EXHAUSTED"
            | "CANDIDATE_CACHE_INVALID"
            | "external_build_failed" => Self::ExternalBuildFailed,
            "GENERATOR_MODEL_OUTPUT_UNSAFE"
            | "GENERATOR_MODEL_OUTPUT_TRUNCATED"
            | "GENERATOR_MODEL_OUTPUT_INVALID"
            | "external_output_rejected" => Self::ExternalOutputRejected,
            "HARNESS_UNAVAILABLE" | "CONNECTION_REFUSED" | "TIMEOUT" | "external_unavailable" => {
                Self::ExternalUnavailable
            }
            "GENERATOR_MODEL_NOT_CONFIGURED"
            | "GENERATOR_NOT_CONFIGURED_FOR_PROFILE"
            | "UNKNOWN_COMPONENT_PROFILE"
            | "INVALID_DEVELOPMENT_REQUEST"
            | "UNSUPPORTED_TARGET_KIND"
            | "external_configuration_missing" => Self::ExternalConfigurationMissing,
            _ => Self::ExternalInfrastructureFailure,
        }
    }

    /// Parse a failure class from a string found in error messages.
    /// This is used by the coding_delivery layer for backwards compatibility.
    pub fn from_message(message: &str) -> Self {
        if message.contains("ACCEPTANCE_KIT_SELECTION_REQUIRED") {
            Self::ExternalInputRequired
        } else if message.contains("ACCEPTANCE_KIT_DIGEST_MISMATCH") {
            Self::ExternalBindingMismatch
        } else if message.contains("GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED")
            || message.contains("CROSS_KIT_CONTAMINATION_FAILED")
            || message.contains("COMBINED_CONTRACT_PROBE_FAILED")
        {
            Self::ExternalVerificationFailed
        } else if message.contains("GENERATOR_COMPILE_REPAIR_EXHAUSTED")
            || message.contains("CANDIDATE_CACHE_INVALID")
        {
            Self::ExternalBuildFailed
        } else if message.contains("GENERATOR_MODEL_OUTPUT_UNSAFE")
            || message.contains("GENERATOR_MODEL_OUTPUT_TRUNCATED")
        {
            Self::ExternalOutputRejected
        } else if message.contains("CONNECT")
            || message.contains("HARNESS_UNAVAILABLE")
            || message.contains("GENERATOR_MODEL_UNAVAILABLE")
            || message.contains("CANDIDATE_NOT_ACCEPTED")
        {
            Self::ExternalUnavailable
        } else if message.contains("GENERATOR_MODEL_NOT_CONFIGURED")
            || message.contains("GENERATOR_NOT_CONFIGURED_FOR_PROFILE")
            || message.contains("UNKNOWN_COMPONENT_PROFILE")
            || message.contains("INVALID_DEVELOPMENT_REQUEST")
        {
            Self::ExternalConfigurationMissing
        } else if message.contains("SANDBOX")
            || message.contains("CODING_ACCEPTANCE_INFRASTRUCTURE_FAILURE")
            || message.contains("GENERATOR_COMPILE_PROBE_INFRASTRUCTURE_FAILURE")
        {
            Self::ExternalInfrastructureFailure
        } else {
            Self::ExternalInfrastructureFailure
        }
    }

    /// Return a safe user-facing message for this failure class.
    pub fn user_facing(&self) -> &'static str {
        match self {
            Self::ExternalInputRequired => "无法确定该开发请求应使用的验收规格，未开始候选生成。",
            Self::ExternalReferenceNotFound => "外部验收规格引用不存在或无效。",
            Self::ExternalBindingMismatch => "验收规格摘要与请求绑定不一致，开发已安全停止。",
            Self::ExternalVerificationFailed => {
                "候选程序未通过业务验收，已安全停止，未创建部署提案。"
            }
            Self::ExternalBuildFailed => "代码生成已完成，但候选程序在编译修复次数耗尽后仍未通过。",
            Self::ExternalOutputRejected => "候选程序违反安全限制，已安全拒绝，未创建部署提案。",
            Self::ExternalUnavailable => "模型生成服务暂时不可用，请稍后重试。",
            Self::ExternalConfigurationMissing => "Coding Harness 配置缺失或不支持该组件类型。",
            Self::ExternalInfrastructureFailure => "基础设施故障，请稍后重试。",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_required_maps_to_input_required() {
        assert_eq!(
            ExternalExecutionFailureClass::from_harness_code("ACCEPTANCE_KIT_SELECTION_REQUIRED"),
            ExternalExecutionFailureClass::ExternalInputRequired
        );
    }

    #[test]
    fn digest_mismatch_maps_to_binding_mismatch() {
        assert_eq!(
            ExternalExecutionFailureClass::from_harness_code("ACCEPTANCE_KIT_DIGEST_MISMATCH"),
            ExternalExecutionFailureClass::ExternalBindingMismatch
        );
    }

    #[test]
    fn harness_codes_roundtrip_consistently() {
        let codes = [
            "ACCEPTANCE_KIT_SELECTION_REQUIRED",
            "GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED",
            "GENERATOR_COMPILE_REPAIR_EXHAUSTED",
            "HARNESS_UNAVAILABLE",
            "GENERATOR_MODEL_NOT_CONFIGURED",
        ];
        for code in &codes {
            let class = ExternalExecutionFailureClass::from_harness_code(code);
            let serialized = class.as_str();
            assert!(!serialized.is_empty());
            let reparsed = ExternalExecutionFailureClass::from_harness_code(serialized);
            assert_eq!(class, reparsed, "roundtrip failed for code: {code}");
        }
    }

    #[test]
    fn user_facing_messages_are_non_empty() {
        let variants = [
            ExternalExecutionFailureClass::ExternalInputRequired,
            ExternalExecutionFailureClass::ExternalReferenceNotFound,
            ExternalExecutionFailureClass::ExternalBindingMismatch,
            ExternalExecutionFailureClass::ExternalVerificationFailed,
            ExternalExecutionFailureClass::ExternalBuildFailed,
            ExternalExecutionFailureClass::ExternalOutputRejected,
            ExternalExecutionFailureClass::ExternalUnavailable,
            ExternalExecutionFailureClass::ExternalConfigurationMissing,
            ExternalExecutionFailureClass::ExternalInfrastructureFailure,
        ];
        for variant in &variants {
            assert!(!variant.user_facing().is_empty());
        }
    }
}
