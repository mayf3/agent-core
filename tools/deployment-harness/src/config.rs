use anyhow::{bail, Result};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DeploymentHarnessConfig {
    pub listen_addr: SocketAddr,
    pub artifact_root: PathBuf,
    pub state_root: PathBuf,
    pub control_token: String,
    pub event_observe_url: String,
    pub event_observe_token: String,
}

impl DeploymentHarnessConfig {
    pub fn from_env() -> Result<Self> {
        let listen_addr = std::env::var("DEPLOYMENT_HARNESS_LISTEN_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:7400".into())
            .parse()?;
        let artifact_root = required_path("DEPLOYMENT_HARNESS_ARTIFACT_ROOT")?;
        let state_root = required_path("DEPLOYMENT_HARNESS_STATE_ROOT")?;
        let control_token = required_secret("DEPLOYMENT_HARNESS_CONTROL_TOKEN")?;
        let event_observe_url = std::env::var("DEPLOYMENT_HARNESS_EVENT_OBSERVE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4130/v1/events".into());
        let event_observe_token = required_secret("DEPLOYMENT_HARNESS_EVENT_OBSERVE_TOKEN")?;
        let config = Self {
            listen_addr,
            artifact_root,
            state_root,
            control_token,
            event_observe_url,
            event_observe_token,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.listen_addr.ip().is_loopback() {
            bail!("DEPLOYMENT_HARNESS_LISTEN_MUST_BE_LOOPBACK");
        }
        validate_token(&self.control_token)?;
        validate_token(&self.event_observe_token)?;
        if self.control_token == self.event_observe_token {
            bail!("DEPLOYMENT_HARNESS_TOKENS_MUST_DIFFER");
        }
        validate_loopback_url(&self.event_observe_url, "/v1/events")?;
        ensure_safe_root(&self.artifact_root)?;
        ensure_safe_root(&self.state_root)?;
        Ok(())
    }
}

fn required_path(name: &str) -> Result<PathBuf> {
    let value = std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"))?;
    if value.trim().is_empty() {
        bail!("{name} is required");
    }
    Ok(PathBuf::from(value))
}

fn required_secret(name: &str) -> Result<String> {
    let value = std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"))?;
    validate_token(&value)?;
    Ok(value)
}

fn validate_token(value: &str) -> Result<()> {
    if value.len() < 32 || value.len() > 512 || value.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        bail!("DEPLOYMENT_HARNESS_TOKEN_INVALID");
    }
    Ok(())
}

fn validate_loopback_url(value: &str, expected_path: &str) -> Result<()> {
    let remainder = value
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("EVENT_OBSERVE_URL_INVALID"))?;
    let (authority, path) = remainder
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("EVENT_OBSERVE_URL_INVALID"))?;
    if format!("/{path}") != expected_path || authority.contains('@') {
        bail!("EVENT_OBSERVE_URL_INVALID");
    }
    let addresses: Vec<SocketAddr> = authority.to_socket_addrs()?.collect();
    if addresses.is_empty() || addresses.iter().any(|address| !loopback(address.ip())) {
        bail!("EVENT_OBSERVE_URL_NOT_LOOPBACK");
    }
    Ok(())
}

fn loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

fn ensure_safe_root(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        bail!("DEPLOYMENT_HARNESS_ROOT_INVALID");
    }
    std::fs::create_dir_all(path)?;
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("DEPLOYMENT_HARNESS_ROOT_SYMLINK");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_observer_and_weak_tokens() {
        assert!(validate_loopback_url("http://192.0.2.1:4130/v1/events", "/v1/events").is_err());
        assert!(validate_token("short").is_err());
    }
}
