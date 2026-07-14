//! Capability Host configuration — all values come from environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

const MIN_TOKEN_LENGTH: usize = 32;

/// Configuration for the Capability Host process.
pub struct CapabilityHostConfig {
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
    /// Bearer token accepted only by the deployment control endpoint.
    pub control_token: String,
    /// Bearer token accepted only by the artifact execution endpoint.
    pub execution_token: String,
}

impl CapabilityHostConfig {
    pub fn from_env() -> Result<Self, String> {
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

        let control_token = std::env::var("CAPABILITY_HOST_CONTROL_TOKEN")
            .map_err(|_| "CAPABILITY_HOST_CONTROL_TOKEN is required".to_string())?;
        let execution_token = std::env::var("CAPABILITY_HOST_EXECUTION_TOKEN")
            .map_err(|_| "CAPABILITY_HOST_EXECUTION_TOKEN is required".to_string())?;

        let config = Self {
            listen_addr,
            artifact_root,
            exec_timeout: Duration::from_millis(exec_timeout_ms),
            max_stdout_bytes,
            max_stderr_bytes,
            control_token,
            execution_token,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        let address: SocketAddr = self
            .listen_addr
            .parse()
            .map_err(|_| "CAPABILITY_HOST_LISTEN_ADDR must be an IP socket address".to_string())?;
        if !address.ip().is_loopback() {
            return Err("CAPABILITY_HOST_LISTEN_ADDR must be loopback".into());
        }
        validate_token("CAPABILITY_HOST_CONTROL_TOKEN", &self.control_token)?;
        validate_token("CAPABILITY_HOST_EXECUTION_TOKEN", &self.execution_token)?;
        if self.control_token == self.execution_token {
            return Err("Capability Host control and execution tokens must differ".into());
        }
        if self.exec_timeout < Duration::from_millis(100)
            || self.exec_timeout > Duration::from_secs(120)
        {
            return Err("CAPABILITY_HOST_EXEC_TIMEOUT_MS is out of range".into());
        }
        for (name, value) in [
            ("CAPABILITY_HOST_MAX_STDOUT_BYTES", self.max_stdout_bytes),
            ("CAPABILITY_HOST_MAX_STDERR_BYTES", self.max_stderr_bytes),
        ] {
            if value == 0 || value > 1024 * 1024 {
                return Err(format!("{name} is out of range"));
            }
        }
        Ok(())
    }
}

fn validate_token(name: &str, token: &str) -> Result<(), String> {
    if token.len() < MIN_TOKEN_LENGTH
        || token.len() > 512
        || token.chars().any(|character| {
            character.is_whitespace() || character.is_control() || !character.is_ascii()
        })
    {
        return Err(format!("{name} must be 32-512 printable ASCII characters"));
    }
    Ok(())
}
