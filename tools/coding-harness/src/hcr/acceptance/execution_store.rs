//! Persistent idempotent execution record store with OS file locking (H7).
//!
//! Each acceptance execution is stored at:
//!   HARNESS_ARTIFACT_ROOT/executions/<full_key_sha256>/
//!
//! Contents:
//!   lock         — OS advisory file lock (auto-released on crash)
//!   request.json — Fingerprint + request parameters
//!   result.json  — Completed result (written atomically: temp → fsync → rename)
//!
//! Concurrency: exclusive `fs2::FileExt::lock_exclusive` on the lock file.
//! Process crash automatically releases the OS lock — no permanent blocking.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde_json::Value;

use super::protocol::RequestFingerprint;

#[derive(Debug)]
pub enum ExecutionStoreError {
    LockFailed(String),
    FingerprintMismatch(String),
    Io(std::io::Error),
    Serde(serde_json::Error),
    CorruptResult(String),
}

impl std::fmt::Display for ExecutionStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionStoreError::LockFailed(e) => write!(f, "lock failed: {e}"),
            ExecutionStoreError::FingerprintMismatch(e) => write!(f, "fingerprint mismatch: {e}"),
            ExecutionStoreError::Io(e) => write!(f, "I/O: {e}"),
            ExecutionStoreError::Serde(e) => write!(f, "serde: {e}"),
            ExecutionStoreError::CorruptResult(e) => write!(f, "corrupt result: {e}"),
        }
    }
}

impl From<std::io::Error> for ExecutionStoreError {
    fn from(e: std::io::Error) -> Self {
        ExecutionStoreError::Io(e)
    }
}
impl From<serde_json::Error> for ExecutionStoreError {
    fn from(e: serde_json::Error) -> Self {
        ExecutionStoreError::Serde(e)
    }
}

/// Execution store with OS-level file locking.
#[derive(Debug, Clone)]
pub struct ExecutionStore {
    root: PathBuf,
}

impl ExecutionStore {
    pub fn new(artifact_root: &Path) -> Self {
        ExecutionStore {
            root: artifact_root.join("executions"),
        }
    }

    /// Full SHA-256 hex of the idempotency key.
    fn key_dir(&self, key: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        self.root.join(hex::encode(Sha256::digest(key.as_bytes())))
    }

    /// Execute an acceptance run under exclusive OS file lock.
    ///
    /// 1. Creates/locks execution directory exclusively.
    /// 2. If completed result exists with matching fingerprint → returns it
    ///    without calling `exec_fn`.
    /// 3. Otherwise calls `exec_fn`, persists result atomically.
    ///
    /// The lock is auto-released on process crash (OS kernel cleanup).
    pub fn execute<F>(
        &self,
        key: &str,
        fingerprint: &RequestFingerprint,
        exec_fn: F,
    ) -> Result<Value, ExecutionStoreError>
    where
        F: FnOnce() -> Result<Value, String>,
    {
        let dir = self.key_dir(key);
        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(&dir)?;

        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&dir.join("lock"))?;
        lock_file
            .lock_exclusive()
            .map_err(|e| ExecutionStoreError::LockFailed(e.to_string()))?;

        let result = self.execute_locked(&dir, fingerprint, exec_fn);
        let _ = lock_file.unlock();
        result
    }

    fn execute_locked<F>(
        &self,
        dir: &Path,
        fingerprint: &RequestFingerprint,
        exec_fn: F,
    ) -> Result<Value, ExecutionStoreError>
    where
        F: FnOnce() -> Result<Value, String>,
    {
        let result_path = dir.join("result.json");
        let req_path = dir.join("request.json");

        // 1. Check for existing completed result
        if result_path.exists() {
            return self.verify_and_load(dir, fingerprint);
        }

        // 2. Clean stale temp
        let _ = fs::remove_file(&dir.join(".result.tmp"));

        // 3. Write request record
        let req = serde_json::json!({
            "schema_version": "1",
            "request_fingerprint": fingerprint.0,
            "claimed_at": chrono::Utc::now().to_rfc3339(),
        });
        fs::write(&req_path, serde_json::to_vec_pretty(&req)?)?;

        // 4. Run gates
        let result = exec_fn().unwrap_or_else(
            |e| serde_json::json!({"error": e, "overall_outcome": "InfrastructureFailure"}),
        );

        // 5. Atomic write: temp → fsync → rename
        let tmp = dir.join(".result.tmp");
        let bytes = serde_json::to_vec_pretty(&result)?;
        {
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &result_path)?;
        if let Some(p) = result_path.parent() {
            if let Ok(f) = File::open(p) {
                let _ = f.sync_all();
            }
        }
        Ok(result)
    }

    fn verify_and_load(
        &self,
        dir: &Path,
        fingerprint: &RequestFingerprint,
    ) -> Result<Value, ExecutionStoreError> {
        let result_path = dir.join("result.json");
        if result_path.is_symlink() {
            return Err(ExecutionStoreError::CorruptResult("symlink".into()));
        }
        let content = fs::read_to_string(&result_path)?;
        if content.trim().is_empty() {
            return Err(ExecutionStoreError::CorruptResult("empty".into()));
        }
        let _result: Value = serde_json::from_str(&content)?;

        // Check request fingerprint
        let req_path = dir.join("request.json");
        if req_path.exists() {
            let rc = fs::read_to_string(&req_path)?;
            if let Ok(r) = serde_json::from_str::<Value>(&rc) {
                let sfp = r
                    .get("request_fingerprint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if sfp != fingerprint.0 {
                    return Err(ExecutionStoreError::FingerprintMismatch(format!(
                        "expected {expected} got {sfp}",
                        expected = fingerprint.0
                    )));
                }
            }
        }
        Ok(_result)
    }

    /// Read-only load (no lock).
    pub fn load_completed(&self, key: &str) -> Option<Value> {
        let dir = self.key_dir(key);
        let p = dir.join("result.json");
        if p.exists() && p.is_file() && !p.is_symlink() {
            fs::read_to_string(&p)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        } else {
            None
        }
    }
}
