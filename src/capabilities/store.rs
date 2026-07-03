//! Content-addressed immutable store for artifact / manifest / evidence blobs.
//! Objects are stored by SHA-256 digest under a configurable root directory.
//! Only readable through verified digest lookups — no arbitrary file paths.
//!
//! On Unix/macOS the load path uses O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC and
//! verifies the opened fd is a regular file before reading. All reads are
//! bounded to MAX_OBJECT_SIZE to prevent unbounded allocation.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

/// Maximum size of a single content-addressed object in bytes.
const MAX_OBJECT_SIZE: usize = 1 * 1024 * 1024; // 1 MiB

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

    /// Load a blob by digest using O_NOFOLLOW, regular-file check, bounded read.
    /// This prevents symlink following, FIFO blocking, and unbounded allocation.
    pub fn load(&self, digest: &Sha256Digest) -> Result<Vec<u8>> {
        let dir = self.object_dir(digest);
        let path = dir.join("object");

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        anyhow::anyhow!("content_object_not_found:{}", digest.as_str())
                    } else {
                        anyhow::anyhow!("content_open_failed:{e}")
                    }
                })?;

            // Verify the opened fd is a regular file (not FIFO, socket, device).
            let meta = file
                .metadata()
                .map_err(|e| anyhow::anyhow!("content_metadata_failed:{e}"))?;
            if !meta.is_file() {
                bail!("content_object_not_regular_file:{}", digest.as_str());
            }

            // Validate file size before reading.
            let file_len = meta.len() as usize;
            if file_len > MAX_OBJECT_SIZE {
                bail!("content_object_too_large:{} bytes", file_len);
            }

            // Bounded read: never allocate more than MAX_OBJECT_SIZE + 1.
            let mut data = Vec::with_capacity(file_len.min(MAX_OBJECT_SIZE));
            let mut reader = file.take((MAX_OBJECT_SIZE + 1) as u64);
            let n = reader
                .read_to_end(&mut data)
                .map_err(|e| anyhow::anyhow!("content_read_failed:{e}"))?;

            // If read returned more than MAX_OBJECT_SIZE, reject.
            if n > MAX_OBJECT_SIZE {
                bail!("content_object_exceeded_limit:{}", digest.as_str());
            }

            if !digest.verify(&data) {
                bail!("content_digest_mismatch:{}", digest.as_str());
            }
            Ok(data)
        }

        #[cfg(not(unix))]
        {
            // Non-Unix fallback: bounded read after metadata check.
            let file = std::fs::File::open(&path)
                .map_err(|_| anyhow::anyhow!("content_object_not_found:{}", digest.as_str()))?;
            let meta = file.metadata()?;
            if !meta.is_file() {
                bail!("content_object_not_regular_file:{}", digest.as_str());
            }
            let file_len = meta.len() as usize;
            if file_len > MAX_OBJECT_SIZE {
                bail!("content_object_too_large:{} bytes", file_len);
            }
            let mut data = Vec::with_capacity(file_len.min(MAX_OBJECT_SIZE));
            let mut reader = file.take((MAX_OBJECT_SIZE + 1) as u64);
            let n = reader.read_to_end(&mut data)?;
            if n > MAX_OBJECT_SIZE {
                bail!("content_object_exceeded_limit:{}", digest.as_str());
            }
            if !digest.verify(&data) {
                bail!("content_digest_mismatch:{}", digest.as_str());
            }
            Ok(data)
        }
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
