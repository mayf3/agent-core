//! HCR-specific error types and error codes.
//!
//! Every HCR execution error maps to a structured error code that can be
//! returned in the harness response envelope. The error codes are designed
//! to be model-parseable and actionable.

use std::fmt;

/// HCR execution errors with structured codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HcrError {
    /// No supported sandbox backend is available on this platform.
    SandboxUnavailable,
    /// The requested command is not in the profile's allowlist.
    CommandNotAllowed,
    /// The requested command's network policy prohibits this operation.
    NetworkDenied,
    /// A path argument resolves outside the workspace root.
    PathOutsideWorkspace,
    /// The child process could not be spawned.
    SpawnFailed(String),
    /// The child process exceeded its time limit.
    Timeout,
    /// Cleanup of the child process (process group, wait) failed.
    CleanupFailed(String),
    /// The requested profile does not exist or is not configured.
    ProfileNotFound,
    /// The HCR token is missing or does not match the configured token.
    TokenRequired,
    /// The command name was not provided or is empty.
    MissingCommand,
    /// A required parameter for a command template was missing.
    MissingParameter(String),
    /// A parameter value was rejected by validation.
    InvalidParameter(String),
    /// Internal harness error during execution.
    Internal(String),
}

impl HcrError {
    /// Return the structured error code string for the harness response envelope.
    pub fn error_code(&self) -> &'static str {
        match self {
            HcrError::SandboxUnavailable => "HCR_SANDBOX_UNAVAILABLE",
            HcrError::CommandNotAllowed => "HCR_COMMAND_NOT_ALLOWED",
            HcrError::NetworkDenied => "HCR_NETWORK_DENIED",
            HcrError::PathOutsideWorkspace => "HCR_PATH_OUTSIDE_WORKSPACE",
            HcrError::SpawnFailed(_) => "HCR_SPAWN_FAILED",
            HcrError::Timeout => "HCR_TIMEOUT",
            HcrError::CleanupFailed(_) => "HCR_CLEANUP_FAILED",
            HcrError::ProfileNotFound => "HCR_PROFILE_NOT_FOUND",
            HcrError::TokenRequired => "HCR_TOKEN_REQUIRED",
            HcrError::MissingCommand => "HCR_MISSING_COMMAND",
            HcrError::MissingParameter(_) => "HCR_MISSING_PARAMETER",
            HcrError::InvalidParameter(_) => "HCR_INVALID_PARAMETER",
            HcrError::Internal(_) => "HCR_INTERNAL_ERROR",
        }
    }
}

impl fmt::Display for HcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HcrError::SandboxUnavailable => write!(f, "sandbox backend unavailable"),
            HcrError::CommandNotAllowed => write!(f, "command not in HCR allowlist"),
            HcrError::NetworkDenied => write!(f, "network access denied by HCR policy"),
            HcrError::PathOutsideWorkspace => write!(f, "path resolves outside workspace"),
            HcrError::SpawnFailed(msg) => write!(f, "spawn failed: {msg}"),
            HcrError::Timeout => write!(f, "execution timed out"),
            HcrError::CleanupFailed(msg) => write!(f, "child cleanup failed: {msg}"),
            HcrError::ProfileNotFound => write!(f, "HCR profile not found"),
            HcrError::TokenRequired => write!(f, "HCR token required"),
            HcrError::MissingCommand => write!(f, "missing HCR command name"),
            HcrError::MissingParameter(p) => write!(f, "missing parameter: {p}"),
            HcrError::InvalidParameter(p) => write!(f, "invalid parameter: {p}"),
            HcrError::Internal(msg) => write!(f, "internal HCR error: {msg}"),
        }
    }
}

impl std::error::Error for HcrError {}
