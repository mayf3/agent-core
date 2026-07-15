use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEPLOYMENT_PROTOCOL: &str = "deployment.effect.v0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentIntent {
    pub protocol_version: String,
    pub invocation_id: String,
    pub intent_id: String,
    pub proposal_id: String,
    pub decision_id: String,
    pub service_manifest_digest: String,
    pub artifact_digest: String,
    pub expected_version: String,
    pub action: String,
}

impl DeploymentIntent {
    pub fn expected_intent_id(&self) -> String {
        stable_id(
            "deployment_intent",
            &serde_json::json!({
                "protocol_version": self.protocol_version,
                "invocation_id": self.invocation_id,
                "proposal_id": self.proposal_id,
                "decision_id": self.decision_id,
                "service_manifest_digest": self.service_manifest_digest,
                "artifact_digest": self.artifact_digest,
                "expected_version": self.expected_version,
                "action": self.action,
            }),
        )
    }

    pub fn validate(&self) -> Result<()> {
        if self.protocol_version != DEPLOYMENT_PROTOCOL || self.action != "install_start" {
            bail!("DEPLOYMENT_INTENT_PROTOCOL_INVALID");
        }
        for value in [
            &self.invocation_id,
            &self.intent_id,
            &self.proposal_id,
            &self.decision_id,
            &self.expected_version,
        ] {
            if value.is_empty()
                || value.len() > 256
                || value
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
            {
                bail!("DEPLOYMENT_INTENT_IDENTITY_INVALID");
            }
        }
        for digest in [&self.service_manifest_digest, &self.artifact_digest] {
            if digest.len() != 71
                || !digest.starts_with("sha256:")
                || !digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
                || digest[7..].bytes().any(|byte| byte.is_ascii_uppercase())
            {
                bail!("DEPLOYMENT_INTENT_DIGEST_INVALID");
            }
        }
        if self.intent_id != self.expected_intent_id() {
            bail!("DEPLOYMENT_INTENT_ID_INVALID");
        }
        Ok(())
    }

    pub fn deployment_id(&self, component_id: &str) -> String {
        stable_id(
            "deployment",
            &serde_json::json!({
                "intent": self,
                "component_id": component_id,
            }),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentReceipt {
    pub protocol_version: String,
    pub receipt_id: String,
    pub invocation_id: String,
    pub intent_id: String,
    pub proposal_id: String,
    pub decision_id: String,
    pub deployment_id: String,
    pub component_id: String,
    pub service_manifest_digest: String,
    pub artifact_digest: String,
    pub version: String,
    pub status: String,
    pub endpoint: String,
    pub health_status: String,
    pub log_ref: String,
    pub previous_artifact_digest: Option<String>,
    pub started_at: String,
    pub finished_at: String,
    pub replayed: bool,
}

impl DeploymentReceipt {
    pub fn expected_receipt_id(&self) -> String {
        stable_id(
            "deployment_receipt",
            &serde_json::json!({
                "invocation_id": self.invocation_id,
                "intent_id": self.intent_id,
                "proposal_id": self.proposal_id,
                "decision_id": self.decision_id,
                "deployment_id": self.deployment_id,
                "component_id": self.component_id,
                "service_manifest_digest": self.service_manifest_digest,
                "artifact_digest": self.artifact_digest,
                "version": self.version,
                "status": self.status,
                "endpoint": self.endpoint,
                "health_status": self.health_status,
                "log_ref": self.log_ref,
                "previous_artifact_digest": self.previous_artifact_digest,
            }),
        )
    }

    pub fn validate_for(&self, intent: &DeploymentIntent, component_id: &str) -> Result<()> {
        if self.protocol_version != DEPLOYMENT_PROTOCOL
            || self.invocation_id != intent.invocation_id
            || self.intent_id != intent.intent_id
            || self.proposal_id != intent.proposal_id
            || self.decision_id != intent.decision_id
            || self.deployment_id != intent.deployment_id(component_id)
            || self.component_id != component_id
            || self.service_manifest_digest != intent.service_manifest_digest
            || self.artifact_digest != intent.artifact_digest
            || self.version != intent.expected_version
            || self.status != "healthy"
            || self.health_status != "ready"
            || !loopback_endpoint(&self.endpoint)
            || !safe_relative_ref(&self.log_ref)
            || self
                .previous_artifact_digest
                .as_deref()
                .is_some_and(|digest| !lower_sha256(digest))
            || self.receipt_id != self.expected_receipt_id()
        {
            bail!("DEPLOYMENT_RECEIPT_BINDING_INVALID");
        }
        let started = chrono::DateTime::parse_from_rfc3339(&self.started_at)
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_RECEIPT_TIME_INVALID"))?;
        let finished = chrono::DateTime::parse_from_rfc3339(&self.finished_at)
            .map_err(|_| anyhow::anyhow!("DEPLOYMENT_RECEIPT_TIME_INVALID"))?;
        if finished < started {
            bail!("DEPLOYMENT_RECEIPT_TIME_INVALID");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentControlIntent {
    pub protocol_version: String,
    pub decision_id: String,
    pub decision_nonce: String,
    pub principal_id: String,
    pub component_id: String,
    pub action: String,
    pub expected_component_snapshot_id: String,
    pub expected_deployment_id: String,
}

impl ComponentControlIntent {
    pub fn expected_decision_id(&self) -> String {
        stable_id(
            "component_control_decision",
            &serde_json::json!({
                "protocol_version": self.protocol_version,
                "decision_nonce": self.decision_nonce,
                "principal_id": self.principal_id,
                "component_id": self.component_id,
                "action": self.action,
                "expected_component_snapshot_id": self.expected_component_snapshot_id,
                "expected_deployment_id": self.expected_deployment_id,
            }),
        )
    }

    pub fn validate(&self) -> Result<()> {
        if self.protocol_version != DEPLOYMENT_PROTOCOL
            || !matches!(self.action.as_str(), "disable" | "rollback")
            || !self.principal_id.starts_with("feishu:open_id:")
            || self.principal_id.len() > 256
            || self.decision_nonce.len() < 32
            || self.decision_nonce.len() > 160
            || !safe_component_id(&self.component_id)
            || !safe_identity(&self.expected_component_snapshot_id, 160)
            || !safe_identity(&self.expected_deployment_id, 160)
            || self.decision_id != self.expected_decision_id()
        {
            bail!("COMPONENT_CONTROL_INTENT_INVALID");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentControlReceipt {
    pub protocol_version: String,
    pub ok: bool,
    pub receipt_id: String,
    pub action: String,
    pub decision_id: String,
    pub component_id: String,
    pub deployment_id: String,
    pub artifact_digest: String,
    pub version: String,
    pub status: String,
    pub endpoint: String,
    pub health_status: String,
    pub log_ref: String,
}

impl ComponentControlReceipt {
    pub fn expected_receipt_id(&self) -> String {
        stable_id(
            "component_control_receipt",
            &serde_json::json!({
                "action": self.action,
                "decision_id": self.decision_id,
                "component_id": self.component_id,
                "deployment_id": self.deployment_id,
                "artifact_digest": self.artifact_digest,
                "version": self.version,
                "status": self.status,
                "endpoint": self.endpoint,
                "health_status": self.health_status,
                "log_ref": self.log_ref,
            }),
        )
    }

    pub fn validate_for(&self, intent: &ComponentControlIntent) -> Result<()> {
        let expected_state = match intent.action.as_str() {
            "disable" => ("disabled", "unavailable"),
            "rollback" => ("rolled_back", "ready"),
            _ => bail!("COMPONENT_CONTROL_RECEIPT_INVALID"),
        };
        if self.protocol_version != DEPLOYMENT_PROTOCOL
            || !self.ok
            || self.action != intent.action
            || self.decision_id != intent.decision_id
            || self.component_id != intent.component_id
            || (intent.action == "disable" && self.deployment_id != intent.expected_deployment_id)
            || !lower_sha256(&self.artifact_digest)
            || !semver_v0(&self.version)
            || self.status != expected_state.0
            || self.health_status != expected_state.1
            || !loopback_endpoint(&self.endpoint)
            || !safe_relative_ref(&self.log_ref)
            || self.receipt_id != self.expected_receipt_id()
        {
            bail!("COMPONENT_CONTROL_RECEIPT_INVALID");
        }
        Ok(())
    }
}

fn lower_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_identity(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| !byte.is_ascii_whitespace() && !byte.is_ascii_control())
}

fn safe_component_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn semver_v0(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 9 && part.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn loopback_endpoint(value: &str) -> bool {
    value
        .strip_prefix("http://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0)
}

fn safe_relative_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
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

fn stable_id(prefix: &str, value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{prefix}_{}", hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_and_receipt_ids_are_action_bound() {
        let intent = DeploymentIntent {
            protocol_version: DEPLOYMENT_PROTOCOL.into(),
            invocation_id: "invocation_1".into(),
            intent_id: String::new(),
            proposal_id: "proposal_1".into(),
            decision_id: "decision_1".into(),
            service_manifest_digest: format!("sha256:{}", "a".repeat(64)),
            artifact_digest: format!("sha256:{}", "b".repeat(64)),
            expected_version: "0.1.0".into(),
            action: "install_start".into(),
        };
        let mut intent = intent;
        intent.intent_id = intent.expected_intent_id();
        intent.validate().unwrap();
        assert_eq!(
            intent.deployment_id("service"),
            intent.deployment_id("service")
        );
        assert_ne!(
            intent.deployment_id("service"),
            intent.deployment_id("other")
        );
    }

    #[test]
    fn component_control_identity_is_action_and_snapshot_bound() {
        let mut intent = ComponentControlIntent {
            protocol_version: DEPLOYMENT_PROTOCOL.into(),
            decision_id: String::new(),
            decision_nonce: "n".repeat(32),
            principal_id: "feishu:open_id:owner".into(),
            component_id: "dashboard".into(),
            action: "disable".into(),
            expected_component_snapshot_id: "component_snap_1".into(),
            expected_deployment_id: "deployment_1".into(),
        };
        intent.decision_id = intent.expected_decision_id();
        intent.validate().unwrap();
        let original = intent.decision_id.clone();
        intent.action = "rollback".into();
        assert_ne!(intent.expected_decision_id(), original);
    }
}
