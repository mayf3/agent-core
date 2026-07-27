//! Generic, content-addressed contract for the pre-model context hook.
//!
//! The Kernel validates bindings and bytes. It does not interpret artifact
//! contents or any context-selection strategy.

use crate::capabilities::store::Sha256Digest;
use anyhow::{bail, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImmutableArtifactRef {
    pub id: String,
    pub digest: String,
}

impl ImmutableArtifactRef {
    pub fn new(id: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            id: id.into(),
            digest: Sha256Digest::compute(bytes).as_str().to_string(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("immutable_ref_id_empty");
        }
        Sha256Digest::parse(&self.digest)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpaqueArtifactRef {
    pub id: String,
    pub digest: String,
    pub media_type: String,
    /// Self-contained transport form. The Kernel decodes and hashes these
    /// bytes but never interprets their media type or content.
    pub content_hex: String,
}

impl OpaqueArtifactRef {
    pub fn new(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        let digest = Sha256Digest::compute(bytes).as_str().to_string();
        Self {
            id: format!("artifact:{digest}"),
            digest,
            media_type: media_type.into(),
            content_hex: hex::encode(bytes),
        }
    }

    pub fn decode_verified(&self) -> Result<Vec<u8>> {
        if self.id.trim().is_empty() || self.media_type.trim().is_empty() {
            bail!("artifact_ref_required_field_empty");
        }
        let digest = Sha256Digest::parse(&self.digest)?;
        let bytes = hex::decode(&self.content_hex).map_err(|_| anyhow::anyhow!("artifact_hex"))?;
        if !digest.verify(&bytes) {
            bail!("artifact_digest_mismatch");
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateInputRef {
    pub run_id: String,
    pub session_id: String,
    pub scope_digest: String,
    pub artifact: OpaqueArtifactRef,
    pub immutable_refs: Vec<ImmutableArtifactRef>,
    pub immutable_refs_digest: String,
}

impl CandidateInputRef {
    pub fn validate(&self) -> Result<()> {
        if self.run_id.trim().is_empty() || self.session_id.trim().is_empty() {
            bail!("candidate_binding_empty");
        }
        Sha256Digest::parse(&self.scope_digest)?;
        self.artifact.decode_verified()?;
        if self.immutable_refs.is_empty() {
            bail!("immutable_refs_empty");
        }
        for item in &self.immutable_refs {
            item.validate()?;
        }
        if digest_immutable_refs(&self.immutable_refs) != self.immutable_refs_digest {
            bail!("immutable_refs_digest_mismatch");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextHookRequest {
    pub request_id: String,
    pub candidate: CandidateInputRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextHookResponse {
    pub run_id: String,
    pub session_id: String,
    pub scope_digest: String,
    pub candidate_digest: String,
    pub immutable_refs: Vec<ImmutableArtifactRef>,
    pub immutable_refs_digest: String,
    /// One or more ordered, opaque artifacts. Ordering is interpreted only by
    /// the selected Model Adapter.
    pub artifacts: Vec<OpaqueArtifactRef>,
}

impl ContextHookResponse {
    pub fn validate_against(&self, request: &ContextHookRequest) -> Result<()> {
        let candidate = &request.candidate;
        if self.run_id != candidate.run_id || self.session_id != candidate.session_id {
            bail!("context_response_run_session_mismatch");
        }
        if self.scope_digest != candidate.scope_digest {
            bail!("context_response_scope_mismatch");
        }
        if self.candidate_digest != candidate.artifact.digest {
            bail!("context_response_candidate_mismatch");
        }
        if self.immutable_refs != candidate.immutable_refs
            || self.immutable_refs_digest != candidate.immutable_refs_digest
            || digest_immutable_refs(&self.immutable_refs) != self.immutable_refs_digest
        {
            bail!("context_response_immutable_refs_mismatch");
        }
        if self.artifacts.is_empty() {
            bail!("context_response_artifacts_empty");
        }
        for artifact in &self.artifacts {
            artifact.decode_verified()?;
        }
        Ok(())
    }

    pub fn authentication_message(&self, provider_id: &str, request_id: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in [
            provider_id,
            request_id,
            &self.run_id,
            &self.session_id,
            &self.scope_digest,
            &self.candidate_digest,
            &self.immutable_refs_digest,
        ] {
            append_field(&mut bytes, field.as_bytes());
        }
        for artifact in &self.artifacts {
            append_field(&mut bytes, artifact.digest.as_bytes());
        }
        bytes
    }
}

#[derive(Debug, Clone)]
pub struct AuthenticatedContextHookResponse {
    /// Comes from the trusted local binding after proof verification. It is
    /// never accepted from response JSON.
    pub provider_id: String,
    pub request_id: String,
    pub response: ContextHookResponse,
}

pub fn digest_immutable_refs(refs: &[ImmutableArtifactRef]) -> String {
    let mut hasher = Sha256::new();
    for item in refs {
        hasher.update((item.id.len() as u64).to_be_bytes());
        hasher.update(item.id.as_bytes());
        hasher.update((item.digest.len() as u64).to_be_bytes());
        hasher.update(item.digest.as_bytes());
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub fn compute_provider_proof(secret: &str, message: &[u8]) -> Result<String> {
    if secret.is_empty() {
        bail!("context_provider_credential_empty");
    }
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| anyhow::anyhow!("hmac_key"))?;
    mac.update(message);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

pub fn verify_provider_proof(secret: &str, message: &[u8], proof: &str) -> Result<()> {
    let proof = hex::decode(proof).map_err(|_| anyhow::anyhow!("provider_proof_encoding"))?;
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| anyhow::anyhow!("hmac_key"))?;
    mac.update(message);
    mac.verify_slice(&proof)
        .map_err(|_| anyhow::anyhow!("provider_authentication_failed"))
}

fn append_field(target: &mut Vec<u8>, field: &[u8]) {
    target.extend_from_slice(&(field.len() as u64).to_be_bytes());
    target.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_proof_is_bound_to_artifact_order() {
        let response = ContextHookResponse {
            run_id: "run".into(),
            session_id: "session".into(),
            scope_digest: Sha256Digest::compute(b"scope").as_str().into(),
            candidate_digest: Sha256Digest::compute(b"candidate").as_str().into(),
            immutable_refs: vec![ImmutableArtifactRef::new("required", b"required")],
            immutable_refs_digest: String::new(),
            artifacts: vec![
                OpaqueArtifactRef::new("a", b"one"),
                OpaqueArtifactRef::new("a", b"two"),
            ],
        };
        let mut response = response;
        response.immutable_refs_digest = digest_immutable_refs(&response.immutable_refs);
        let proof = compute_provider_proof(
            "secret",
            &response.authentication_message("provider", "request"),
        )
        .unwrap();
        assert!(verify_provider_proof(
            "secret",
            &response.authentication_message("provider", "request"),
            &proof
        )
        .is_ok());
        response.artifacts.swap(0, 1);
        assert!(verify_provider_proof(
            "secret",
            &response.authentication_message("provider", "request"),
            &proof
        )
        .is_err());
    }
}
