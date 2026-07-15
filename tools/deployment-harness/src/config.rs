use anyhow::{bail, Result};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
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
        let artifact_root = required_root("DEPLOYMENT_HARNESS_ARTIFACT_ROOT")?;
        let state_root = required_root("DEPLOYMENT_HARNESS_STATE_ROOT")?;
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
        if self.artifact_root == self.state_root
            || self.artifact_root.starts_with(&self.state_root)
            || self.state_root.starts_with(&self.artifact_root)
        {
            bail!("DEPLOYMENT_HARNESS_ROOTS_OVERLAP");
        }
        Ok(())
    }
}

fn required_root(name: &str) -> Result<PathBuf> {
    let value = std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"))?;
    if value.trim().is_empty() {
        bail!("{name} is required");
    }
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path.parent().is_none()
        || path
            .ancestors()
            .any(|ancestor| ancestor.join(".git").is_dir())
    {
        bail!("{name} is unsafe");
    }
    std::fs::create_dir_all(&path)?;
    Ok(path.canonicalize()?)
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
    if path.as_os_str().is_empty() || !path.is_absolute() || path.parent().is_none() {
        bail!("DEPLOYMENT_HARNESS_ROOT_INVALID");
    }
    std::fs::create_dir_all(path)?;
    if std::fs::symlink_metadata(path)?.file_type().is_symlink()
        || path
            .ancestors()
            .any(|ancestor| ancestor.join(".git").is_dir())
        || std::env::current_dir()
            .ok()
            .and_then(|current| current.canonicalize().ok())
            .as_deref()
            == Some(path)
        || std::env::var_os("HOME")
            .and_then(|home| PathBuf::from(home).canonicalize().ok())
            .as_deref()
            == Some(path)
    {
        bail!("DEPLOYMENT_HARNESS_ROOT_UNSAFE");
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
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

    #[test]
    fn roots_must_be_absolute_canonical_and_disjoint() {
        assert!(ensure_safe_root(Path::new("relative")).is_err());
        assert!(ensure_safe_root(Path::new("/")).is_err());

        let root = tempfile::TempDir::new().unwrap();
        let artifact_root = root.path().join("artifacts");
        let state_root = artifact_root.join("state");
        std::fs::create_dir_all(&state_root).unwrap();
        let config = DeploymentHarnessConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            artifact_root,
            state_root,
            control_token: "c".repeat(32),
            event_observe_url: "http://127.0.0.1:4130/v1/events".into(),
            event_observe_token: "o".repeat(32),
        };
        assert!(config.validate().is_err());
    }
}
