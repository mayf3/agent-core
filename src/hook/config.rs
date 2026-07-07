//! Hook ABI v0 configuration types.
//!
//! `HookConfig` describes a single hook binding (kind, endpoint, limits,
//! failure mode). `HookRegistryConfig` holds the full set of configured
//! hooks. Both types carry `serde` derives so they can be deserialised
//! from JSON or TOML (e.g. `hook_registry.json`).

use serde::{Deserialize, Serialize};

use crate::hook::{HookEndpoint, HookFailureMode, HookKind, HookLimits, HookValidationError};

/// Configuration for a single hook binding.
///
/// # Default
///
/// The default is a **disabled** hook with safe resource limits — it will
/// never be invoked at runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    /// When `false`, this hook is never invoked. **Default: false.**
    pub enabled: bool,
    /// Identifies the lifecycle point this hook binds to.
    pub kind: HookKind,
    /// Transport endpoint for this hook.
    pub endpoint: HookEndpoint,
    /// Maximum wall-clock time for the hook call, in milliseconds.
    /// Default 5000.
    pub timeout_ms: u64,
    /// Maximum serialised request body size in bytes. Default 1 MiB.
    pub max_request_bytes: u64,
    /// Maximum serialised response body size in bytes. Default 1 MiB.
    pub max_response_bytes: u64,
    /// Maximum number of `ContextFragment` entries the hook may return.
    /// Default 20.
    pub max_fragments: usize,
    /// Behaviour when the hook call fails. **Default: `disabled`.**
    pub failure_mode: HookFailureMode,
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: HookKind::IngressRouteV0,
            endpoint: HookEndpoint { url: String::new() },
            timeout_ms: 5_000,
            max_request_bytes: 1024 * 1024,
            max_response_bytes: 1024 * 1024,
            max_fragments: 20,
            failure_mode: HookFailureMode::Disabled,
        }
    }
}

impl HookConfig {
    /// Returns `Ok(())` if the configuration is internally consistent and
    /// within safety bounds.
    ///
    /// Checks performed:
    /// - If `enabled`, the endpoint URL must be non-empty.
    /// - If `enabled`, `failure_mode` must not be `Disabled`.
    /// - Limits are validated against hard-coded maxima.
    pub fn validate(&self) -> Result<(), HookValidationError> {
        if !self.enabled {
            return Ok(());
        }
        if self.endpoint.url.trim().is_empty() {
            return Err(HookValidationError::Invalid {
                message: "enabled hook must have a non-empty endpoint URL".into(),
            });
        }
        if self.failure_mode == HookFailureMode::Disabled {
            return Err(HookValidationError::Invalid {
                message: "enabled hook must not have failure_mode = disabled".into(),
            });
        }
        let limits = HookLimits {
            timeout_ms: self.timeout_ms,
            max_request_bytes: self.max_request_bytes,
            max_response_bytes: self.max_response_bytes,
            max_fragments: self.max_fragments,
        };
        limits.validate()
    }
}

/// Root configuration for the hook registry.
///
/// Serialised as `hook_registry.json` in the Kernel data directory.
///
/// # Default
///
/// The default is a disabled registry with no hooks configured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookRegistryConfig {
    /// Master switch. When `false`, no hooks are invoked regardless of
    /// individual `HookConfig.enabled` values. **Default: false.**
    pub enabled: bool,
    /// The list of configured hook bindings.
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
}

impl Default for HookRegistryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hooks: Vec::new(),
        }
    }
}

impl HookRegistryConfig {
    /// Returns all enabled hook configs (respects the master switch).
    pub fn active_hooks(&self) -> Vec<&HookConfig> {
        if !self.enabled {
            return Vec::new();
        }
        self.hooks.iter().filter(|h| h.enabled).collect()
    }

    /// Validates every hook in the registry.
    pub fn validate(&self) -> Result<(), HookValidationError> {
        for hook in &self.hooks {
            hook.validate()?;
        }
        Ok(())
    }
}
