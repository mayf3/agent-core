//! Narrow authenticated client for Capability Host deployment preparation.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityDeployRequest {
    pub protocol_version: String,
    pub proposal_id: String,
    pub decision_id: String,
    pub manifest_digest: String,
    pub artifact_digest: String,
    pub target_registry_snapshot_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CapabilityDeployResult {
    pub deployment_id: String,
    pub proposal_id: String,
    pub decision_id: String,
    pub manifest_digest: String,
    pub manifest_id: String,
    pub artifact_digest: String,
    pub operation_name: String,
    pub target_registry_snapshot_id: String,
    pub probe_execution_id: String,
}

pub trait CapabilityHostDeployer {
    fn deploy(&self, request: &CapabilityDeployRequest) -> Result<CapabilityDeployResult>;
}

#[derive(Debug, thiserror::Error)]
#[error("capability host definitively rejected deployment")]
pub struct DefinitiveDeploymentRejection;

pub fn is_definitive_rejection(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<DefinitiveDeploymentRejection>()
        .is_some()
}

pub struct HttpCapabilityHostClient {
    address: SocketAddr,
    host_header: String,
    control_token: String,
    timeout: Duration,
}

impl HttpCapabilityHostClient {
    pub fn from_env() -> Result<Self> {
        let endpoint = std::env::var("AGENT_CORE_CAPABILITY_HOST_CONTROL_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7300".into());
        let control_token = std::env::var("AGENT_CORE_CAPABILITY_HOST_CONTROL_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_HOST_CONTROL_NOT_CONFIGURED"))?;
        Self::new(&endpoint, control_token, Duration::from_secs(15))
    }

    pub fn new(endpoint: &str, control_token: String, timeout: Duration) -> Result<Self> {
        if control_token.trim().is_empty() {
            bail!("CAPABILITY_HOST_CONTROL_NOT_CONFIGURED");
        }
        let authority = endpoint
            .strip_prefix("http://")
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_HOST_ENDPOINT_INVALID"))?
            .trim_end_matches('/');
        if authority.contains('/') || authority.contains('@') {
            bail!("CAPABILITY_HOST_ENDPOINT_INVALID");
        }
        let addresses: Vec<SocketAddr> = authority
            .to_socket_addrs()
            .map_err(|_| anyhow::anyhow!("CAPABILITY_HOST_ENDPOINT_INVALID"))?
            .collect();
        if addresses.is_empty() || addresses.iter().any(|address| !is_loopback(address.ip())) {
            bail!("CAPABILITY_HOST_ENDPOINT_NOT_LOOPBACK");
        }
        Ok(Self {
            address: addresses[0],
            host_header: authority.into(),
            control_token,
            timeout,
        })
    }
}

impl CapabilityHostDeployer for HttpCapabilityHostClient {
    fn deploy(&self, request: &CapabilityDeployRequest) -> Result<CapabilityDeployResult> {
        let body = serde_json::to_vec(request)?;
        let wire = format!(
            "POST /deploy HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.host_header,
            self.control_token,
            body.len(),
            String::from_utf8_lossy(&body),
        );
        let mut stream = TcpStream::connect_timeout(&self.address, self.timeout)
            .map_err(|_| anyhow::anyhow!("CAPABILITY_HOST_CONNECT_FAILED"))?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream
            .write_all(wire.as_bytes())
            .map_err(|_| anyhow::anyhow!("CAPABILITY_HOST_WRITE_FAILED"))?;
        let mut raw = Vec::new();
        stream
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut raw)?;
        if raw.len() > MAX_RESPONSE_BYTES {
            bail!("CAPABILITY_HOST_RESPONSE_TOO_LARGE");
        }
        let response = String::from_utf8(raw)
            .map_err(|_| anyhow::anyhow!("CAPABILITY_HOST_RESPONSE_INVALID"))?;
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(0);
        let payload = response
            .split_once("\r\n\r\n")
            .map(|(_, payload)| payload)
            .ok_or_else(|| anyhow::anyhow!("CAPABILITY_HOST_RESPONSE_INVALID"))?;
        let value: serde_json::Value = serde_json::from_str(payload)
            .map_err(|_| anyhow::anyhow!("CAPABILITY_HOST_RESPONSE_INVALID"))?;
        if !(200..300).contains(&status) {
            // Only a valid, authenticated Host 400 proves that validation or
            // the pre-persist probe rejected the candidate. Transport errors,
            // 5xx, 409 and malformed responses are uncertain: the Host may
            // already have durably prepared this exact deployment, so the
            // Approval must remain Pending for an identity-bound retry.
            if status == 400
                && value.get("protocol_version").and_then(|v| v.as_str())
                    == Some("capability-deploy-v1")
                && value.get("ok").and_then(|v| v.as_bool()) == Some(false)
                && value
                    .get("error_code")
                    .and_then(|v| v.as_str())
                    .is_some_and(|code| {
                        !code.is_empty()
                            && code.len() <= 64
                            && code
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    })
            {
                return Err(DefinitiveDeploymentRejection.into());
            }
            bail!("CAPABILITY_HOST_DEPLOY_UNCERTAIN");
        }
        if value.get("protocol_version").and_then(|v| v.as_str()) != Some("capability-deploy-v1")
            || value.get("ok").and_then(|v| v.as_bool()) != Some(true)
        {
            bail!("CAPABILITY_HOST_DEPLOY_FAILED");
        }
        serde_json::from_value(value)
            .map_err(|_| anyhow::anyhow!("CAPABILITY_HOST_RESPONSE_INVALID"))
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_and_empty_token() {
        assert!(HttpCapabilityHostClient::new(
            "http://192.0.2.10:7300",
            "token".into(),
            Duration::from_secs(1)
        )
        .is_err());
        assert!(HttpCapabilityHostClient::new(
            "http://127.0.0.1:7300",
            String::new(),
            Duration::from_secs(1)
        )
        .is_err());
    }
}
