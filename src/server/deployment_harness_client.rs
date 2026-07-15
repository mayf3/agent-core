//! Narrow authenticated client for the external managed-service Deployment Harness.

use crate::domain::{
    ComponentControlIntent, ComponentControlReceipt, DeploymentIntent, DeploymentReceipt,
    DEPLOYMENT_PROTOCOL,
};
use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 128 * 1024;

pub trait DeploymentHarnessDeployer {
    fn deploy(&self, intent: &DeploymentIntent) -> Result<DeploymentReceipt>;
}

pub trait DeploymentHarnessController {
    fn control(&self, intent: &ComponentControlIntent) -> Result<ComponentControlReceipt>;
}

#[derive(Debug, thiserror::Error)]
#[error("deployment harness definitively rejected intent")]
pub struct DefinitiveDeploymentRejection;

pub fn is_definitive_rejection(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<DefinitiveDeploymentRejection>()
        .is_some()
}

pub struct HttpDeploymentHarnessClient {
    address: SocketAddr,
    host_header: String,
    control_token: String,
    timeout: Duration,
}

impl HttpDeploymentHarnessClient {
    pub fn from_env() -> Result<Self> {
        let endpoint = std::env::var("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7400".into());
        let token = std::env::var("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_TOKEN")
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_CONTROL_NOT_CONFIGURED"))?;
        Self::new(&endpoint, token, Duration::from_secs(30))
    }

    pub fn new(endpoint: &str, control_token: String, timeout: Duration) -> Result<Self> {
        if control_token.len() < 32
            || control_token.len() > 512
            || control_token.bytes().any(|byte| byte.is_ascii_whitespace())
        {
            bail!("DEPLOYMENT_HARNESS_CONTROL_NOT_CONFIGURED");
        }
        let authority = endpoint
            .strip_prefix("http://")
            .ok_or_else(|| anyhow::anyhow!("DEPLOYMENT_HARNESS_ENDPOINT_INVALID"))?
            .trim_end_matches('/');
        if authority.contains('/') || authority.contains('@') {
            bail!("DEPLOYMENT_HARNESS_ENDPOINT_INVALID");
        }
        let addresses: Vec<SocketAddr> = authority
            .to_socket_addrs()
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_ENDPOINT_INVALID"))?
            .collect();
        if addresses.is_empty() || addresses.iter().any(|address| !loopback(address.ip())) {
            bail!("DEPLOYMENT_HARNESS_ENDPOINT_NOT_LOOPBACK");
        }
        Ok(Self {
            address: addresses[0],
            host_header: authority.into(),
            control_token,
            timeout,
        })
    }

    fn post(&self, path: &str, body: &[u8]) -> Result<Vec<u8>> {
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.host_header,
            self.control_token,
            body.len(),
        );
        let mut stream = TcpStream::connect_timeout(&self.address, self.timeout)
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_CONNECT_FAILED"))?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream
            .write_all(head.as_bytes())
            .and_then(|_| stream.write_all(body))
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_WRITE_FAILED"))?;
        let mut raw = Vec::new();
        stream
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut raw)?;
        if raw.len() > MAX_RESPONSE_BYTES {
            bail!("DEPLOYMENT_HARNESS_RESPONSE_TOO_LARGE");
        }
        let response = String::from_utf8(raw)
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_RESPONSE_INVALID"))?;
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(0);
        let payload = response
            .split_once("\r\n\r\n")
            .map(|(_, payload)| payload.as_bytes().to_vec())
            .ok_or_else(|| anyhow::anyhow!("DEPLOYMENT_HARNESS_RESPONSE_INVALID"))?;
        if !(200..300).contains(&status) {
            let value: serde_json::Value = serde_json::from_slice(&payload)
                .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_RESPONSE_INVALID"))?;
            if matches!(status, 400 | 409 | 422)
                && value.get("protocol_version").and_then(|v| v.as_str())
                    == Some(DEPLOYMENT_PROTOCOL)
                && value.get("ok").and_then(|v| v.as_bool()) == Some(false)
                && value
                    .get("error_code")
                    .and_then(|v| v.as_str())
                    .is_some_and(safe_error_code)
            {
                return Err(DefinitiveDeploymentRejection.into());
            }
            bail!("DEPLOYMENT_HARNESS_EFFECT_UNCERTAIN");
        }
        Ok(payload)
    }
}

impl DeploymentHarnessDeployer for HttpDeploymentHarnessClient {
    fn deploy(&self, intent: &DeploymentIntent) -> Result<DeploymentReceipt> {
        intent.validate()?;
        let body = serde_json::to_vec(intent)?;
        let payload = self.post("/v1/deployments", &body)?;
        let receipt: DeploymentReceipt = serde_json::from_slice(&payload)
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_RESPONSE_INVALID"))?;
        Ok(receipt)
    }
}

impl DeploymentHarnessController for HttpDeploymentHarnessClient {
    fn control(&self, intent: &ComponentControlIntent) -> Result<ComponentControlReceipt> {
        intent.validate()?;
        let path = format!("/v1/components/{}/{}", intent.component_id, intent.action);
        let body = serde_json::to_vec(&serde_json::json!({
            "decision_id": intent.decision_id,
        }))?;
        let payload = self.post(&path, &body)?;
        let receipt: ComponentControlReceipt = serde_json::from_slice(&payload)
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_HARNESS_RESPONSE_INVALID"))?;
        receipt.validate_for(intent)?;
        Ok(receipt)
    }
}

fn safe_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_rejects_remote_endpoint_and_weak_token() {
        assert!(HttpDeploymentHarnessClient::new(
            "http://192.0.2.1:7400",
            "x".repeat(32),
            Duration::from_secs(1),
        )
        .is_err());
        assert!(HttpDeploymentHarnessClient::new(
            "http://127.0.0.1:7400",
            "weak".into(),
            Duration::from_secs(1),
        )
        .is_err());
    }
}
