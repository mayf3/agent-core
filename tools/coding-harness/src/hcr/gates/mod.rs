//! HCR acceptance gates for candidate verification.
//!
//! Provides five gates:
//! - Scaffold: Check candidate structure and manifest validity
//! - Build: Build the candidate with cargo
//! - TrustedTest: Run system-provided tests against the candidate
//! - TrustedSmoke: Run the candidate entry point and verify output
//! - Artifact: Validate build artifact and manifest consistency
//!
//! All gates operate on an immutable `CandidateSnapshot` with digest
//! verification before and after each gate execution.
//!
//! The build gate creates a writable copy of the candidate source in the
//! shared work directory. Subsequent gates (TrustedTest, TrustedSmoke)
//! locate the built binary in the shared build output.
//!
//! # Security
//!
//! - Candidate digest is enforced before AND after every gate. Any change
//!   aborts the entire acceptance run (H1).
//! - Sandbox execution fails closed: no host fallback when sandbox is
//!   unavailable (B2).

pub mod artifact;
pub mod build;
pub mod scaffold;
pub mod trusted_smoke;
pub mod trusted_test;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hcr::candidate::{verify_digest, CandidateSnapshot};
use crate::hcr::executor::CleanupStatus;
use crate::hcr::process;
use crate::hcr::sandbox::{self, SandboxBackend, SandboxConfig};
use serde_json::json;

/// The kind of acceptance gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    Scaffold,
    Build,
    TrustedTest,
    TrustedSmoke,
    Artifact,
}

impl GateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GateKind::Scaffold => "scaffold",
            GateKind::Build => "build",
            GateKind::TrustedTest => "trusted_test",
            GateKind::TrustedSmoke => "trusted_smoke",
            GateKind::Artifact => "artifact",
        }
    }
}

/// The result of a single gate execution.
#[derive(Debug, Clone)]
pub struct GateResult {
    pub gate_kind: GateKind,
    pub passed: bool,
    pub is_candidate_failure: bool,
    pub exit_code: i32,
    pub timed_out: bool,
    pub child_cleanup: CleanupStatus,
    pub error_code: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub candidate_digest_preserved: bool,
    /// Real computed SHA-256 of the build artifact (set by Artifact gate).
    /// `None` if no artifact was validated or the gate failed.
    pub computed_artifact_digest: Option<String>,
}

impl GateResult {
    /// Serialize to a JSON value suitable for reporting.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "gate_kind": self.gate_kind.as_str(),
            "passed": self.passed,
            "is_candidate_failure": self.is_candidate_failure,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "child_cleanup": self.child_cleanup.as_str(),
            "error_code": self.error_code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "candidate_id": self.candidate_id,
            "candidate_digest": self.candidate_digest,
            "candidate_digest_preserved": self.candidate_digest_preserved,
            "computed_artifact_digest": self.computed_artifact_digest,
        })
    }

    /// Create a failed result for infrastructure errors.
    pub fn infrastructure_failure(
        gate_kind: GateKind,
        error_code: &str,
        message: &str,
        candidate: &CandidateSnapshot,
    ) -> Self {
        GateResult {
            gate_kind,
            passed: false,
            is_candidate_failure: false,
            exit_code: -1,
            timed_out: false,
            child_cleanup: CleanupStatus::Confirmed,
            error_code: Some(error_code.to_string()),
            stdout: String::new(),
            stderr: message.to_string(),
            candidate_id: candidate.candidate_id.clone(),
            candidate_digest: candidate.candidate_digest.clone(),
            candidate_digest_preserved: false,
    computed_artifact_digest: None,
        }
    }
}

/// Shared context passed between gates during execution.
///
/// Gates that produce build artifacts (Build) communicate the binary
/// location to gates that consume them (TrustedTest, TrustedSmoke).
#[derive(Debug, Clone)]
pub(crate) struct GateContext {
    /// The shared work directory root.
    pub work_base: PathBuf,
    /// Path to the candidate source copy for building (writable).
    pub build_source: PathBuf,
    /// Path to the build output target directory.
    pub build_target: PathBuf,
    /// Path to the built candidate binary (populated by Build gate).
    pub built_binary: PathBuf,
}

impl GateContext {
    pub fn new(work_base: PathBuf, candidate: &CandidateSnapshot) -> Self {
        let build_source = work_base.join("build_src");
        let build_target = work_base.join("target");
        let built_binary = work_base.join("target/release/calculator-harness");
        GateContext {
            work_base,
            build_source,
            build_target,
            built_binary,
        }
    }
}

// ── Digest enforcement: re-export the error message ──
/// Error message emitted when candidate digest changes between gate checks.
pub const CANDIDATE_INTEGRITY_VIOLATION: &str = "CANDIDATE_INTEGRITY_VIOLATION";

/// Run all five acceptance gates against the given candidate snapshot.
///
/// Returns a vector of 0–5 `GateResult`s. If the candidate digest changes
/// before or during any gate, execution aborts immediately and later gates
/// are not executed (H1 enforcement).
pub fn run_all_gates(candidate: &CandidateSnapshot) -> Vec<GateResult> {
    let expected_digest = &candidate.candidate_digest;

    let work_base = std::env::temp_dir().join(format!(
        "hcr_gates_work_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let ctx = GateContext::new(work_base.clone(), candidate);

    // Helper to run a gate with mandatory digest verification.
    // Returns None (abort) if digest changed.
    let run_gate = |gate_kind: GateKind,
                    gate_fn: fn(&CandidateSnapshot, &GateContext) -> GateResult|
     -> Option<GateResult> {
        // Verify digest BEFORE gate — any change aborts acceptance.
        let actual_before = verify_digest(candidate).unwrap_or(false);
        if !actual_before {
            let mut result = GateResult::infrastructure_failure(
                gate_kind,
                CANDIDATE_INTEGRITY_VIOLATION,
                &format!(
                    "candidate digest changed before {} gate: expected {}, observed changed",
                    gate_kind.as_str(),
                    expected_digest,
                ),
                candidate,
            );
            result.candidate_digest_preserved = false;
            return Some(result);
        }

        // Execute the gate
        let mut result = gate_fn(candidate, &ctx);

        // Verify digest AFTER gate — any change aborts acceptance.
        let actual_after = verify_digest(candidate).unwrap_or(false);
        if !actual_after {
            // The gate itself may have produced a result, but the digest
            // changed — this is an infrastructure integrity failure.
            let mut abort = GateResult::infrastructure_failure(
                gate_kind,
                CANDIDATE_INTEGRITY_VIOLATION,
                &format!(
                    "candidate digest changed during/after {} gate: expected {}, observed changed",
                    gate_kind.as_str(),
                    expected_digest,
                ),
                candidate,
            );
            abort.candidate_digest_preserved = false;
            return Some(abort);
        }

        // Digest was preserved
        result.candidate_digest_preserved = true;

        // Override candidate fields from snapshot
        result.candidate_id = candidate.candidate_id.clone();
        result.candidate_digest = candidate.candidate_digest.clone();

        Some(result)
    };

    // Define the gate execution order
    let gates: [(GateKind, fn(&CandidateSnapshot, &GateContext) -> GateResult); 5] = [
        (GateKind::Scaffold, scaffold::check),
        (GateKind::Build, build::check),
        (GateKind::TrustedTest, trusted_test::check),
        (GateKind::TrustedSmoke, trusted_smoke::check),
        (GateKind::Artifact, artifact::check),
    ];

    let mut results = Vec::with_capacity(5);

    for (kind, func) in &gates {
        match run_gate(*kind, *func) {
            Some(r) => {
                let aborted =
                    !r.passed && r.error_code.as_deref() == Some(CANDIDATE_INTEGRITY_VIOLATION);
                results.push(r);
                if aborted {
                    break;
                }
            }
            None => {
                // Shouldn't happen since run_gate always returns Some
                break;
            }
        }
    }

    // Cleanup work base
    let _ = std::fs::remove_dir_all(&work_base);

    results
}

// ── Sandboxed command execution (fail-closed) ──

/// Result of a sandboxed command execution.
#[derive(Debug, Clone)]
pub(crate) struct SandboxedCommandResult {
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub child_cleanup: CleanupStatus,
}

/// Run a command with mandatory sandbox wrapping.
///
/// # Security (B2)
///
/// This function **never** falls back to host execution. If the sandbox
/// backend is unavailable or initialization fails, it returns an
/// `Err` tuple indicating `InfrastructureFailure`. The caller must
/// propagate this as a gate failure.
///
/// On macOS without sandbox-exec, or Linux without bubblewrap, every
/// call returns an error — no candidate code is executed on the host.
pub(crate) fn run_command_sandboxed(
    program: &Path,
    args: &[&str],
    work_dir: &Path,
    timeout: Duration,
    stdin_input: &[&str],
    extra_env: &[(&str, &str)],
) -> Result<SandboxedCommandResult, SandboxedCommandResult> {
    let backend = SandboxBackend::detect();

    let sandbox_config = SandboxConfig {
        workspace_root: work_dir.to_path_buf(),
        home_dir: work_dir.join(".sandbox-home"),
        real_home: process::dirs_fallback(),
        agent_core_repo: process::find_agent_core_repo(),
        network_policy: crate::hcr::profile::NetworkPolicy::Deny,
    };

    // Fail-closed: sandbox must be available
    if let SandboxBackend::Unavailable = backend {
        return Err(SandboxedCommandResult {
            exit_code: -1,
            timed_out: false,
            stdout: String::new(),
            stderr: "sandbox backend unavailable: execution denied (fail-closed)".into(),
            child_cleanup: CleanupStatus::Confirmed,
        });
    }

    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.current_dir(work_dir);

    let sandbox_home = work_dir.join(".sandbox-home");
    let needs_cleanup = !sandbox_home.exists();
    if needs_cleanup {
        let _ = std::fs::create_dir_all(&sandbox_home);
    }

    // Set up environment
    cmd.env_clear();
    if let Some(v) = std::env::var_os("PATH") {
        cmd.env("PATH", v);
    }
    if let Some(v) = std::env::var_os("TMPDIR") {
        cmd.env("TMPDIR", v);
    } else {
        cmd.env("TMPDIR", std::env::temp_dir());
    }
    cmd.env("HOME", &sandbox_home);
    for (key, val) in extra_env {
        cmd.env(key, val);
    }

    // Process group
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // Wrap with sandbox — fail-closed
    let mut cmd = match sandbox::wrap_with_sandbox(&mut cmd, &sandbox_config, &backend) {
        Ok(c) => c,
        Err(e) => {
            if needs_cleanup {
                let _ = std::fs::remove_dir_all(&sandbox_home);
            }
            return Err(SandboxedCommandResult {
                exit_code: -1,
                timed_out: false,
                stdout: String::new(),
                stderr: format!("sandbox init failed (fail-closed): {e}"),
                child_cleanup: CleanupStatus::Confirmed,
            });
        }
    };

    let has_stdin = !stdin_input.is_empty();
    cmd.stdin(if has_stdin {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    })
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if needs_cleanup {
                let _ = std::fs::remove_dir_all(&sandbox_home);
            }
            return Err(SandboxedCommandResult {
                exit_code: -1,
                timed_out: false,
                stdout: String::new(),
                stderr: format!("spawn failed: {e}"),
                child_cleanup: CleanupStatus::Confirmed,
            });
        }
    };

    // Write stdin if provided
    if has_stdin {
        if let Some(mut stdin) = child.stdin.take() {
            for line in stdin_input {
                let _ = std::io::Write::write_all(&mut stdin, line.as_bytes());
            }
        }
    }

    // Drain stdout/stderr
    let out_buf = Arc::new(std::sync::Mutex::new(Vec::new()));
    let err_buf = Arc::new(std::sync::Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));

    if let Some(pipe) = child.stdout.take() {
        let b = Arc::clone(&out_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || process::drain_reader(pipe, b, d, 1_048_576));
    }
    if let Some(pipe) = child.stderr.take() {
        let b = Arc::clone(&err_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || process::drain_reader(pipe, b, d, 1_048_576));
    }

    let start = Instant::now();
    let mut timed_out = false;
    let child_pid = child.id();

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = process::kill_process_tree(child_pid);
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

    let stdout_all = out_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr_all = err_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let stdout_str = String::from_utf8_lossy(&stdout_all).to_string();
    let stderr_str = String::from_utf8_lossy(&stderr_all).to_string();

    let cleanup = if needs_cleanup {
        match std::fs::remove_dir_all(&sandbox_home) {
            Ok(_) => CleanupStatus::Confirmed,
            Err(_) => CleanupStatus::Failed,
        }
    } else {
        CleanupStatus::Confirmed
    };

    Ok(SandboxedCommandResult {
        exit_code,
        timed_out,
        stdout: stdout_str,
        stderr: stderr_str,
        child_cleanup: cleanup,
    })
}
