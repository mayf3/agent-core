use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::TargetKind;

pub const SERVICE_MANIFEST_SCHEMA: &str = "deployment.service-manifest.v0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenPolicy {
    pub host: String,
    pub port: u16,
    pub exposure: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceHealthcheck {
    pub method: String,
    pub path: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradePolicy {
    pub strategy: String,
    pub require_healthy_before_switch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackPolicy {
    pub retain_previous_versions: u8,
    pub automatic_on_health_failure: bool,
}

/// Immutable, content-addressed deployment contract for a managed service.
/// It contains no host path, command arguments, environment values or secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceManifest {
    pub schema_version: String,
    pub manifest_id: String,
    pub component_id: String,
    pub kind: TargetKind,
    pub artifact_digest: String,
    pub entrypoint: String,
    pub runtime_profile: String,
    pub version: String,
    pub required_contracts: Vec<String>,
    pub requested_permissions: Vec<String>,
    pub listen_policy: ListenPolicy,
    pub healthcheck: ServiceHealthcheck,
    pub state_path: String,
    pub upgrade_policy: UpgradePolicy,
    pub rollback_policy: RollbackPolicy,
}

impl ServiceManifest {
    pub fn compute_manifest_id(&self) -> Result<String> {
        let mut canonical = self.clone();
        canonical.manifest_id.clear();
        let digest = Sha256::digest(serde_json::to_vec(&canonical)?);
        Ok(format!("service_manifest_{}", hex::encode(digest)))
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SERVICE_MANIFEST_SCHEMA
            || self.kind != TargetKind::HookConsumerService
            || self.runtime_profile != "managed-service-v0"
            || self.entrypoint != "artifact"
            || !safe_id(&self.component_id)
            || !sha256(&self.artifact_digest)
            || !semver_v0(&self.version)
        {
            bail!("SERVICE_MANIFEST_IDENTITY_INVALID");
        }
        if self.manifest_id != self.compute_manifest_id()? {
            bail!("SERVICE_MANIFEST_DIGEST_INVALID");
        }
        if self.required_contracts != ["event.observe.v0"]
            || self.requested_permissions != ["journal.observe"]
        {
            bail!("SERVICE_MANIFEST_CONTRACT_INVALID");
        }
        if self.listen_policy.host != "127.0.0.1"
            || self.listen_policy.port != 0
            || self.listen_policy.exposure != "loopback"
        {
            bail!("SERVICE_MANIFEST_LISTEN_POLICY_INVALID");
        }
        if self.healthcheck.method != "GET"
            || !safe_absolute_path(&self.healthcheck.path)
            || !(100..=30_000).contains(&self.healthcheck.timeout_ms)
            || !safe_relative_path(&self.state_path)
        {
            bail!("SERVICE_MANIFEST_HEALTH_INVALID");
        }
        if self.upgrade_policy.strategy != "replace_after_ready"
            || !self.upgrade_policy.require_healthy_before_switch
            || !(1..=5).contains(&self.rollback_policy.retain_previous_versions)
        {
            bail!("SERVICE_MANIFEST_LIFECYCLE_POLICY_INVALID");
        }
        Ok(())
    }
}

pub(crate) fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn semver_v0(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 9 && part.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('/')
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
}

fn safe_absolute_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 128
        && value != "/"
        && value[1..].split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> ServiceManifest {
        let mut value = ServiceManifest {
            schema_version: SERVICE_MANIFEST_SCHEMA.into(),
            manifest_id: String::new(),
            component_id: "observer-service".into(),
            kind: TargetKind::HookConsumerService,
            artifact_digest: format!("sha256:{}", "a".repeat(64)),
            entrypoint: "artifact".into(),
            runtime_profile: "managed-service-v0".into(),
            version: "0.1.0".into(),
            required_contracts: vec!["event.observe.v0".into()],
            requested_permissions: vec!["journal.observe".into()],
            listen_policy: ListenPolicy {
                host: "127.0.0.1".into(),
                port: 0,
                exposure: "loopback".into(),
            },
            healthcheck: ServiceHealthcheck {
                method: "GET".into(),
                path: "/health".into(),
                timeout_ms: 5_000,
            },
            state_path: "state".into(),
            upgrade_policy: UpgradePolicy {
                strategy: "replace_after_ready".into(),
                require_healthy_before_switch: true,
            },
            rollback_policy: RollbackPolicy {
                retain_previous_versions: 2,
                automatic_on_health_failure: true,
            },
        };
        value.manifest_id = value.compute_manifest_id().unwrap();
        value
    }

    #[test]
    fn valid_manifest_is_content_addressed_and_strict() {
        let value = manifest();
        value.validate().unwrap();
        let mut changed = value.clone();
        changed.listen_policy.host = "0.0.0.0".into();
        assert!(changed.validate().is_err());
        let mut unknown = serde_json::to_value(value).unwrap();
        unknown["command"] = serde_json::json!("sh -c anything");
        assert!(serde_json::from_value::<ServiceManifest>(unknown).is_err());
    }
}
