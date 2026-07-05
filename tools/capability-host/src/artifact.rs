//! Artifact loading and digest verification.
//!
//! Artifacts are stored in the shared ContentStore under
//! `<artifact_root>/sha256/<first-2>/<next-2>/<full-64>/object`.
//! The Capability Host only reads by digest — never by operation name or path.

use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use std::path::{Path, PathBuf};

/// Locate an artifact file by its digest and verify content integrity.
/// Returns the path to the artifact binary on success.
pub fn resolve_artifact(
    artifact_root: &Path,
    digest_str: &str,
) -> Result<PathBuf, ArtifactError> {
    // Parse the digest string ("sha256:<hex>").
    let digest = Sha256Digest::parse(digest_str).map_err(|_| ArtifactError::InvalidDigest)?;

    // Use the ContentStore to load and verify.
    let store = ContentStore::new(artifact_root.to_path_buf());
    let bytes = store.load(&digest).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") || msg.contains("No such") || msg.contains("not_found") {
            ArtifactError::NotFound
        } else if msg.contains("content mismatch")
            || msg.contains("digest mismatch")
        {
            ArtifactError::DigestMismatch
        } else {
            ArtifactError::StoreError(e.to_string())
        }
    })?;

    // Write the artifact to a temporary executable file.
    // We cannot execute directly from the ContentStore since it uses
    // a structured path layout; copy to a temp file with +x.
    let temp_dir = std::env::temp_dir().join(format!("capability_artifact_{}", digest_str));
    let _ = std::fs::create_dir_all(&temp_dir);
    let artifact_path = temp_dir.join("artifact");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&artifact_path, &bytes).map_err(|e| ArtifactError::StoreError(e.to_string()))?;
        std::fs::set_permissions(&artifact_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| ArtifactError::StoreError(e.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&artifact_path, &bytes)
            .map_err(|e| ArtifactError::StoreError(e.to_string()))?;
    }

    Ok(artifact_path)
}

/// Errors from artifact resolution.
#[derive(Debug)]
pub enum ArtifactError {
    InvalidDigest,
    NotFound,
    DigestMismatch,
    StoreError(String),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactError::InvalidDigest => write!(f, "artifact digest format is invalid"),
            ArtifactError::NotFound => write!(f, "artifact not found in store"),
            ArtifactError::DigestMismatch => write!(f, "artifact content digest mismatch"),
            ArtifactError::StoreError(msg) => write!(f, "artifact store error: {msg}"),
        }
    }
}
