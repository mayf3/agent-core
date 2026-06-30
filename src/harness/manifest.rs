use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// An immutable external harness manifest. Registered once and stored in
/// `harness_manifests` table. Manifests are never modified after creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessManifest {
    pub manifest_id: String,
    pub harness_id: String,
    pub artifact_digest: String,
    pub protocol_version: String,
    pub endpoint: String,
    pub operation_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub idempotent: bool,
    pub created_at: DateTime<Utc>,
}

impl HarnessManifest {
    /// Compute a deterministic manifest ID from the immutable content fields.
    /// `created_at` is excluded since it is set by the system at registration time.
    pub fn compute_manifest_id(&self) -> Result<String> {
        let canonical = serde_json::json!({
            "harness_id": self.harness_id,
            "artifact_digest": self.artifact_digest,
            "protocol_version": self.protocol_version,
            "endpoint": self.endpoint,
            "operation_name": self.operation_name,
            "description": self.description,
            "input_schema": self.input_schema,
            "output_schema": self.output_schema,
            "idempotent": self.idempotent,
        });
        let canonical_json = serde_json::to_string(&canonical)?;
        let mut hasher = Sha256::new();
        hasher.update(canonical_json.as_bytes());
        let digest = hex::encode(hasher.finalize());
        Ok(format!("manifest_{digest}"))
    }

    /// Validate that the endpoint is a localhost loopback address.
    pub fn validate_endpoint(&self) -> Result<()> {
        // Must start with http://
        if !self.endpoint.starts_with("http://") {
            bail!(
                "endpoint scheme must be http, got endpoint starting with {:?}",
                &self.endpoint[..self.endpoint.find(':').unwrap_or(0).min(8)]
            );
        }
        let without_scheme = self.endpoint.trim_start_matches("http://");
        let host = without_scheme
            .split('/')
            .next()
            .unwrap_or(without_scheme)
            .split(':')
            .next()
            .unwrap_or(without_scheme);
        if host != "127.0.0.1" && host != "localhost" && host != "::1" {
            bail!(
                "endpoint host {host:?} is not a loopback address; only 127.0.0.1, localhost, and ::1 are allowed"
            );
        }
        Ok(())
    }

    /// Validate that the operation name starts with "external.".
    pub fn validate_operation_name(&self) -> Result<()> {
        if !self.operation_name.starts_with("external.") {
            bail!(
                "operation name {:?} must start with 'external.'",
                self.operation_name
            );
        }
        Ok(())
    }

    /// Validate artifact_digest format (sha256:...).
    pub fn validate_artifact_digest(&self) -> Result<()> {
        if !self.artifact_digest.starts_with("sha256:") {
            bail!(
                "artifact_digest {:?} must start with 'sha256:'",
                self.artifact_digest
            );
        }
        Ok(())
    }
}
