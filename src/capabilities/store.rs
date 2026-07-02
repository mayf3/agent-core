//! Content-addressed immutable store for artifact / manifest / evidence blobs.
//! Objects are stored by SHA-256 digest under a configurable root directory.
//! Only readable through verified digest lookups — no arbitrary file paths.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::PathBuf;
use std::sync::Mutex;

/// A SHA-256 digest in canonical form: `sha256:<64 lowercase hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != 71 {
            bail!("invalid_digest_length");
        }
        if !s.starts_with("sha256:") {
            bail!("digest_must_start_with_sha256:");
        }
        let hex = &s[7..];
        if hex.len() != 64 {
            bail!("invalid_hex_length");
        }
        if !hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
            bail!("invalid_hex_chars");
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compute the SHA-256 digest of `data` and return the canonical form.
    pub fn compute(data: &[u8]) -> Self {
        let mut h = sha2::Sha256::new();
        h.update(data);
        let hex = hex::encode(h.finalize());
        Self(format!("sha256:{hex}"))
    }

    /// Verify that `data` matches this digest.
    pub fn verify(&self, data: &[u8]) -> bool {
        let computed = Self::compute(data);
        computed.0 == self.0
    }
}

/// Content-addressed store. Thread-safe, digest-keyed, size-bounded.
pub struct ContentStore {
    root: PathBuf,
    total_bytes: Mutex<usize>,
}

impl ContentStore {
    pub fn new(root: PathBuf) -> Self {
        std::fs::create_dir_all(&root).ok();
        Self {
            root,
            total_bytes: Mutex::new(0),
        }
    }

    /// Store a blob and return its digest. Idempotent: same bytes → same digest.
    pub fn store(&self, data: &[u8]) -> Result<Sha256Digest> {
        let digest = Sha256Digest::compute(data);
        let dir = self.object_dir(&digest);
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("object"), data)?;
        }
        let mut total = self.total_bytes.lock().unwrap();
        *total += data.len();
        if *total > 10 * 1024 * 1024 {
            bail!("store_total_size_exceeded");
        }
        Ok(digest)
    }

    /// Load a blob by digest. Returns error if not found or digest mismatch.
    pub fn load(&self, digest: &Sha256Digest) -> Result<Vec<u8>> {
        let dir = self.object_dir(digest);
        let path = dir.join("object");
        if !path.exists() {
            bail!("content_object_not_found:{}", digest.as_str());
        }
        let data = std::fs::read(&path)?;
        if data.len() > 1024 * 1024 {
            bail!("content_object_too_large");
        }
        if !digest.verify(&data) {
            bail!("content_digest_mismatch:{}", digest.as_str());
        }
        Ok(data)
    }

    fn object_dir(&self, digest: &Sha256Digest) -> PathBuf {
        // Directory: <root>/sha256/<hex>/
        let hex = &digest.as_str()[7..]; // strip "sha256:"
        self.root
            .join("sha256")
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(hex)
    }

    /// The on-disk path of the object identified by `digest`. Exposed so tests
    /// can simulate on-disk tampering (then verify `load` rejects it).
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn object_path(&self, digest: &Sha256Digest) -> PathBuf {
        self.object_dir(digest).join("object")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_parse_accepts_valid() {
        let d = Sha256Digest::parse(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert_eq!(
            d.as_str(),
            "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn digest_parse_rejects_bad_length() {
        assert!(Sha256Digest::parse("sha256:too_short").is_err());
        assert!(Sha256Digest::parse(
            "sha256:000000000000000000000000000000000000000000000000000000000000000"
        )
        .is_err());
    }

    #[test]
    fn digest_parse_rejects_uppercase() {
        assert!(Sha256Digest::parse(
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        )
        .is_err());
    }

    #[test]
    fn digest_compute_and_verify() {
        let data = b"hello world";
        let d = Sha256Digest::compute(data);
        assert!(d.verify(data));
        assert!(!d.verify(b"other"));
    }

    #[test]
    fn store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("content_store_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ContentStore::new(dir.join("store"));
        let data = b"test artifact content";
        let d1 = store.store(data).unwrap();
        let loaded = store.load(&d1).unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn store_rejects_tampered() {
        let dir = std::env::temp_dir().join(format!("content_store_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ContentStore::new(dir.join("store"));
        let data = b"original content";
        let d = store.store(data).unwrap();
        // Tamper the stored file.
        let hex = &d.as_str()[7..];
        let path = dir
            .join("store")
            .join("sha256")
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(hex)
            .join("object");
        std::fs::write(path, b"tampered").unwrap();
        assert!(store.load(&d).is_err());
    }
}
