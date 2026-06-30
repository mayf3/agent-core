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

/// Parsed result of a strict localhost HTTP endpoint.
#[derive(Debug, Clone)]
pub struct ParsedEndpoint {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
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

    /// Strictly parse and validate the endpoint URL.
    /// Returns a ParsedEndpoint on success.
    pub fn parse_endpoint(&self) -> Result<ParsedEndpoint> {
        let ep = &self.endpoint;

        // Scheme must be http://
        let without_scheme = ep
            .strip_prefix("http://")
            .ok_or_else(|| anyhow::anyhow!("endpoint must start with http://"))?;

        // No userinfo allowed.
        if without_scheme.contains('@') {
            bail!("endpoint must not contain userinfo");
        }

        // Split host:port from path.
        let (host_port, path) = if let Some(idx) = without_scheme.find('/') {
            (&without_scheme[..idx], &without_scheme[idx..])
        } else {
            (without_scheme, "/")
        };

        // Path must be absolute.
        if !path.starts_with('/') {
            bail!("endpoint path must be absolute");
        }

        // No query or fragment allowed.
        if path.contains('?') {
            bail!("endpoint must not contain query string");
        }
        if path.contains('#') {
            bail!("endpoint must not contain fragment");
        }

        // Parse host and port.
        let (host, port_str) = if let Some(idx) = host_port.find(':') {
            (&host_port[..idx], Some(&host_port[idx + 1..]))
        } else {
            (host_port, None)
        };

        if host.is_empty() {
            bail!("endpoint host is empty");
        }

        // Validate host is loopback.
        if host != "127.0.0.1" && host != "localhost" && host != "[::1]" && host != "::1" {
            bail!(
                "endpoint host {host:?} is not a loopback address; only 127.0.0.1, localhost, and ::1 are allowed"
            );
        }

        // Port must be present and valid.
        let port: u16 = port_str
            .ok_or_else(|| anyhow::anyhow!("endpoint must have an explicit port"))?
            .parse()
            .map_err(|_| anyhow::anyhow!("endpoint port is not a valid u16"))?;

        Ok(ParsedEndpoint {
            scheme: "http".into(),
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }

    /// Validate that the endpoint is a localhost loopback address.
    pub fn validate_endpoint(&self) -> Result<()> {
        self.parse_endpoint()?;
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

    /// Validate artifact_digest format (sha256: + 64 hex chars).
    pub fn validate_artifact_digest(&self) -> Result<()> {
        if !self.artifact_digest.starts_with("sha256:") {
            bail!(
                "artifact_digest {:?} must start with 'sha256:'",
                self.artifact_digest
            );
        }
        let hex_part = &self.artifact_digest[7..];
        if hex_part.len() != 64 || !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!(
                "artifact_digest {:?} must have exactly 64 hex characters after 'sha256:'",
                self.artifact_digest
            );
        }
        Ok(())
    }

    /// Validate protocol_version.
    pub fn validate_protocol_version(&self) -> Result<()> {
        if self.protocol_version != "external-harness-v1" {
            bail!(
                "protocol_version must be 'external-harness-v1', got {:?}",
                self.protocol_version
            );
        }
        Ok(())
    }

    /// Validate that input and output schemas are valid for the strict validator.
    pub fn validate_schemas(&self) -> Result<()> {
        crate::registry::schema::validate_schema_structure(&self.input_schema)
            .map_err(|e| anyhow::anyhow!("input_schema invalid: {e}"))?;
        crate::registry::schema::validate_schema_structure(&self.output_schema)
            .map_err(|e| anyhow::anyhow!("output_schema invalid: {e}"))?;
        Ok(())
    }

    /// Run all validations.
    pub fn validate_all(&self) -> Result<()> {
        self.validate_endpoint()?;
        self.validate_operation_name()?;
        self.validate_artifact_digest()?;
        self.validate_protocol_version()?;
        self.validate_schemas()?;
        Ok(())
    }
}
