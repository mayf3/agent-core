//! Capability Host configuration — all values come from environment variables.
//! No submit/decision tokens are accepted (design boundary).

use std::path::PathBuf;
use std::time::Duration;

/// Configuration for the Capability Host process.
pub(crate) struct CapabilityHostConfig {
    /// Listen address, e.g. "127.0.0.1:7300".
    pub listen_addr: String,
    /// Root directory of the content-addressed artifact store.
    pub artifact_root: PathBuf,
    /// Maximum wall-clock time for artifact execution.
    pub exec_timeout: Duration,
    /// Maximum bytes to read from artifact stdout.
    pub max_stdout_bytes: usize,
    /// Maximum bytes to read from artifact stderr.
    pub max_stderr_bytes: usize,
}

impl CapabilityHostConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let listen_addr = std::env::var("CAPABILITY_HOST_LISTEN_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:7300".to_string());

        let artifact_root = std::env::var("CAPABILITY_HOST_ARTIFACT_ROOT")
            .map(PathBuf::from)
            .map_err(|_| "CAPABILITY_HOST_ARTIFACT_ROOT is required".to_string())?;

        let exec_timeout_ms: u64 = std::env::var("CAPABILITY_HOST_EXEC_TIMEOUT_MS")
            .unwrap_or_else(|_| "30000".to_string())
            .parse()
            .map_err(|_| "CAPABILITY_HOST_EXEC_TIMEOUT_MS must be a valid integer".to_string())?;

        let max_stdout_bytes: usize = std::env::var("CAPABILITY_HOST_MAX_STDOUT_BYTES")
            .unwrap_or_else(|_| "65536".to_string())
            .parse()
            .map_err(|_| "CAPABILITY_HOST_MAX_STDOUT_BYTES must be a valid integer".to_string())?;

        let max_stderr_bytes: usize = std::env::var("CAPABILITY_HOST_MAX_STDERR_BYTES")
            .unwrap_or_else(|_| "65536".to_string())
            .parse()
            .map_err(|_| "CAPABILITY_HOST_MAX_STDERR_BYTES must be a valid integer".to_string())?;

        Ok(Self {
            listen_addr,
            artifact_root,
            exec_timeout: Duration::from_millis(exec_timeout_ms),
            max_stdout_bytes,
            max_stderr_bytes,
        })
    }
}
