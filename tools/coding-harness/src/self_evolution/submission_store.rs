//! Crash-conservative idempotency boundary for an entire development submission.
//!
//! A durable request marker is written before plan/generation/gates begin. A
//! completed response is returned verbatim on replay. If the marker exists but
//! no result can be proved after the OS lock is released, the attempt is sealed
//! as `outcome_unknown`; the handler is never entered again.

use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum SubmissionExecution {
    Completed(Value),
    InProgress,
    OutcomeUnknown,
}

#[derive(Debug, Clone)]
pub struct SubmissionStore {
    root: PathBuf,
}

impl SubmissionStore {
    pub fn new(artifact_root: &Path) -> Self {
        Self {
            root: artifact_root.join("submission-attempts"),
        }
    }

    fn key_dir(&self, key: &str) -> PathBuf {
        self.root.join(hex::encode(Sha256::digest(key.as_bytes())))
    }

    pub fn execute<F>(&self, key: &str, request: &Value, handler: F) -> SubmissionExecution
    where
        F: FnOnce() -> Value,
    {
        match self.execute_inner(key, request, handler) {
            Ok(result) => result,
            Err(_) => SubmissionExecution::OutcomeUnknown,
        }
    }

    fn execute_inner<F>(
        &self,
        key: &str,
        request: &Value,
        handler: F,
    ) -> Result<SubmissionExecution, Box<dyn std::error::Error>>
    where
        F: FnOnce() -> Value,
    {
        fs::create_dir_all(&self.root)?;
        let dir = self.key_dir(key);
        fs::create_dir_all(&dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(dir.join("lock"))?;
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(SubmissionExecution::InProgress)
            }
            Err(error) => return Err(Box::new(error)),
        }

        let result = self.execute_locked(&dir, request, handler);
        let _ = lock.unlock();
        result
    }

    fn execute_locked<F>(
        &self,
        dir: &Path,
        request: &Value,
        handler: F,
    ) -> Result<SubmissionExecution, Box<dyn std::error::Error>>
    where
        F: FnOnce() -> Value,
    {
        let result_path = dir.join("result.json");
        let request_path = dir.join("request.json");
        let fingerprint = fingerprint(request)?;

        if result_path.exists() {
            if persisted_fingerprint(&request_path)? != fingerprint {
                return Ok(SubmissionExecution::OutcomeUnknown);
            }
            return Ok(SubmissionExecution::Completed(read_json(&result_path)?));
        }

        if request_path.exists() {
            // The prior owner released its lock without a durable terminal
            // response. Never re-enter plan, generator, or gates.
            let unknown = outcome_unknown();
            if persisted_fingerprint(&request_path)? == fingerprint {
                write_json_atomic(&result_path, &unknown)?;
            }
            return Ok(SubmissionExecution::OutcomeUnknown);
        }

        write_json_atomic(
            &request_path,
            &json!({
                "schema_version": "1",
                "request_fingerprint": fingerprint,
                "claimed_at": chrono::Utc::now().to_rfc3339(),
            }),
        )?;
        let result = handler();
        write_json_atomic(&result_path, &result)?;
        Ok(SubmissionExecution::Completed(result))
    }
}

fn fingerprint(value: &Value) -> Result<String, serde_json::Error> {
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(value)?))
    ))
}

fn persisted_fingerprint(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(read_json(path)?
        .get("request_fingerprint")
        .and_then(Value::as_str)
        .ok_or("missing request fingerprint")?
        .to_string())
}

fn read_json(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    if path.is_symlink() {
        return Err("submission record is a symlink".into());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let temp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub fn in_progress() -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "outcome": "in_progress",
        "error_code": "SUBMISSION_IN_PROGRESS",
    })
}

pub fn outcome_unknown() -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "outcome": "outcome_unknown",
        "error_code": "SUBMISSION_OUTCOME_UNKNOWN",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    fn root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "coding_submission_store_{label}_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn completed_attempt_replays_without_reentering_whole_handler() {
        let root = root("completed");
        let store = SubmissionStore::new(&root);
        let calls = AtomicUsize::new(0);
        let request = json!({"attempt": "one", "payload": {"same": true}});
        let first = store.execute("attempt-one", &request, || {
            calls.fetch_add(1, Ordering::SeqCst);
            json!({"outcome":"definitively_rejected","ok":false})
        });
        let second = store.execute("attempt-one", &request, || {
            calls.fetch_add(1, Ordering::SeqCst);
            json!({"unexpected":true})
        });
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn concurrent_replay_returns_in_progress_without_entering_handler() {
        let root = root("running");
        let store = SubmissionStore::new(&root);
        let request = json!({"attempt": "running"});
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let owner_store = store.clone();
        let owner_request = request.clone();
        let owner_entered = entered.clone();
        let owner_release = release.clone();
        let owner = std::thread::spawn(move || {
            owner_store.execute("attempt-running", &owner_request, || {
                owner_entered.wait();
                owner_release.wait();
                json!({"outcome":"succeeded","ok":true})
            })
        });
        entered.wait();
        let replay_calls = AtomicUsize::new(0);
        assert_eq!(
            store.execute("attempt-running", &request, || {
                replay_calls.fetch_add(1, Ordering::SeqCst);
                Value::Null
            }),
            SubmissionExecution::InProgress
        );
        assert_eq!(replay_calls.load(Ordering::SeqCst), 0);
        release.wait();
        owner.join().unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn incomplete_attempt_closes_unknown_and_distinct_attempt_executes() {
        let root = root("unknown");
        let store = SubmissionStore::new(&root);
        let request = json!({"payload": "identical"});
        let dir = store.key_dir("attempt-incomplete");
        fs::create_dir_all(&dir).unwrap();
        write_json_atomic(
            &dir.join("request.json"),
            &json!({"request_fingerprint": fingerprint(&request).unwrap()}),
        )
        .unwrap();
        let calls = AtomicUsize::new(0);
        assert_eq!(
            store.execute("attempt-incomplete", &request, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Value::Null
            }),
            SubmissionExecution::OutcomeUnknown
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            store.execute("attempt-new", &request, || {
                calls.fetch_add(1, Ordering::SeqCst);
                json!({"outcome":"succeeded","ok":true})
            }),
            SubmissionExecution::Completed(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn definitive_rejection_does_not_deduplicate_a_distinct_attempt() {
        let root = root("distinct");
        let store = SubmissionStore::new(&root);
        let identical_payload = json!({"development_request": {"same": true}});
        let calls = AtomicUsize::new(0);
        assert!(matches!(
            store.execute("attempt-rejected", &identical_payload, || {
                calls.fetch_add(1, Ordering::SeqCst);
                json!({
                    "protocol_version":"external-harness-v1",
                    "ok":false,
                    "outcome":"definitively_rejected",
                    "error_code":"GENERIC_REJECTION"
                })
            }),
            SubmissionExecution::Completed(_)
        ));
        assert!(matches!(
            store.execute("attempt-revised", &identical_payload, || {
                calls.fetch_add(1, Ordering::SeqCst);
                json!({
                    "protocol_version":"external-harness-v1",
                    "ok":true,
                    "outcome":"succeeded",
                    "result":{}
                })
            }),
            SubmissionExecution::Completed(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        fs::remove_dir_all(root).ok();
    }
}
