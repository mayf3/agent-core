//! Persistent idempotent execution record store.
//!
//! Each acceptance execution is stored at:
//!   HARNESS_ARTIFACT_ROOT/executions/<key_hash>/
//!
//! Contents:
//!   request.json     - Fingerprint + request parameters
//!   result.json      - Completed acceptance result (written atomically)
//!
//! Concurrency: `claim_execution` uses `create_new` directory semantics
//! (std::fs::create_dir is atomic on the OS level). Only one caller
//! succeeds; others get `ExecutionAlreadyClaimed`.

use std::path::{Path, PathBuf};

use super::protocol::{sanitize_key, RequestFingerprint};
use serde_json::Value;

/// Errors from the execution store.
#[derive(Debug)]
pub enum ExecutionStoreError {
    AlreadyClaimed,
    AlreadyCompleted,
    Io(std::io::Error),
    FingerprintMismatch,
    Serde(serde_json::Error),
    Incomplete,
}

impl std::fmt::Display for ExecutionStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionStoreError::AlreadyClaimed => write!(f, "already claimed"),
            ExecutionStoreError::AlreadyCompleted => write!(f, "already completed"),
            ExecutionStoreError::Io(e) => write!(f, "I/O: {e}"),
            ExecutionStoreError::FingerprintMismatch => write!(f, "fingerprint mismatch"),
            ExecutionStoreError::Serde(e) => write!(f, "serde: {e}"),
            ExecutionStoreError::Incomplete => write!(f, "incomplete execution record"),
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

/// A guard representing a claimed execution. Dropping without completing
/// leaves an incomplete record that will NOT be treated as complete.
#[derive(Debug)]
pub struct ExecutionGuard {
    pub dir: PathBuf,
    pub fingerprint: RequestFingerprint,
}

/// Persistent execution record store.
#[derive(Debug, Clone)]
pub struct ExecutionStore {
    root: PathBuf,
}

impl ExecutionStore {
    pub fn new(artifact_root: &Path) -> Self {
        let root = artifact_root.join("executions");
        ExecutionStore { root }
    }

    /// Path for a given idempotency key.
    fn key_path(&self, idempotency_key: &str) -> PathBuf {
        self.root.join(sanitize_key(idempotency_key))
    }

    /// Atomically claim an execution for the given key and fingerprint.
    ///
    /// Uses `std::fs::create_dir` which is atomic on all major OSes:
    /// only one caller succeeds; others get `AlreadyClaimed`.
    ///
    /// If a completed result already exists and the fingerprint matches,
    /// returns `Err(AlreadyCompleted)` — the caller should load and return
    /// the existing result instead of re-executing.
    ///
    /// If a completed result exists with a DIFFERENT fingerprint, returns
    /// `Err(FingerprintMismatch)` — the caller should reject with conflict.
    pub fn claim_execution(
        &self,
        idempotency_key: &str,
        fingerprint: &RequestFingerprint,
    ) -> Result<ExecutionGuard, ExecutionStoreError> {
        let dir = self.key_path(idempotency_key);

        // Check for existing completed result
        let result_path = dir.join("result.json");
        if result_path.exists() {
            let stored: Value = serde_json::from_str(&std::fs::read_to_string(&result_path)?)?;
            let stored_fp = stored
                .get("request_fingerprint")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if stored_fp == fingerprint.0 {
                return Err(ExecutionStoreError::AlreadyCompleted);
            } else {
                return Err(ExecutionStoreError::FingerprintMismatch);
            }
        }

        // Check for incomplete (partial) record — also reject
        if dir.exists() {
            return Err(ExecutionStoreError::AlreadyClaimed);
        }

        // Atomic claim: create_new directory
        match std::fs::create_dir_all(&self.root) {
            Ok(_) => {}
            Err(e) => return Err(ExecutionStoreError::Io(e)),
        }
        match std::fs::create_dir(&dir) {
            Ok(_) => {}
            Err(_) => return Err(ExecutionStoreError::AlreadyClaimed),
        }

        // Write request record
        let request_record = serde_json::json!({
            "request_fingerprint": fingerprint.0,
            "claimed_at": chrono::Utc::now().to_rfc3339(),
        });
        let request_bytes = serde_json::to_vec_pretty(&request_record)?;
        std::fs::write(dir.join("request.json"), &request_bytes)?;

        Ok(ExecutionGuard {
            dir,
            fingerprint: fingerprint.clone(),
        })
    }

    /// Complete an execution by atomically writing the result.
    ///
    /// Uses: temp file → fsync → atomic rename.
    pub fn complete_execution(
        &self,
        guard: &ExecutionGuard,
        result: &Value,
    ) -> Result<(), ExecutionStoreError> {
        let result_path = guard.dir.join("result.json");
        let tmp_path = guard.dir.join(".result.tmp");

        let bytes = serde_json::to_vec_pretty(result)?;
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            use std::io::Write;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, &result_path)?;
        // Sync parent directory
        if let Some(parent) = result_path.parent() {
            if let Ok(f) = std::fs::File::open(parent) {
                let _ = f.sync_all();
            }
        }
        Ok(())
    }

    /// Load a completed result. Returns `None` if no completed result exists.
    pub fn load_completed(&self, idempotency_key: &str) -> Option<Value> {
        let result_path = self.key_path(idempotency_key).join("result.json");
        if result_path.exists() {
            std::fs::read_to_string(&result_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        } else {
            None
        }
    }

    /// Check if a result is still pending/incomplete (request exists but no result).
    pub fn is_pending(&self, idempotency_key: &str) -> bool {
        let dir = self.key_path(idempotency_key);
        dir.join("request.json").exists() && !dir.join("result.json").exists()
    }
}
