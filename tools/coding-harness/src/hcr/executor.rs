//! HCR execution orchestrator.
//!
//! Coordinates command policy validation, environment isolation, sandbox
//! wrapping, process lifecycle, and structured result production for HCR
//! child process execution.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::command::CommandPolicy;
use super::errors::HcrError;
use super::process;
use super::profile::HcrProfile;
use super::sandbox::{self, SandboxBackend, SandboxConfig};

/// The status of an HCR execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HcrStatus {
    Succeeded,
    Failed,
    TimedOut,
    Denied,
}

impl HcrStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            HcrStatus::Succeeded => "succeeded",
            HcrStatus::Failed => "failed",
            HcrStatus::TimedOut => "timed_out",
            HcrStatus::Denied => "denied",
        }
    }
}

/// Child cleanup confirmation status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupStatus {
    Confirmed,
    Failed,
}

impl CleanupStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CleanupStatus::Confirmed => "confirmed",
            CleanupStatus::Failed => "failed",
        }
    }
}

/// Structured result of an HCR execution.
#[derive(Debug, Clone)]
pub struct HcrExecResult {
    pub status: HcrStatus,
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub child_cleanup: CleanupStatus,
    pub error_code: Option<String>,
}

impl HcrExecResult {
    /// Serialize to the JSON response envelope.
    pub fn to_json(&self) -> Value {
        let mut result = json!({
            "status": self.status.as_str(),
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "stdout_truncated": self.stdout_truncated,
            "stderr_truncated": self.stderr_truncated,
            "child_cleanup": self.child_cleanup.as_str(),
        });
        if let Some(ref ec) = self.error_code {
            result["error_code"] = json!(ec);
        }
        json!({
            "protocol_version": "external-harness-v1",
            "ok": self.status == HcrStatus::Succeeded,
            "result": result,
        })
    }
}

/// Execute a named HCR command within the given profile.
///
/// This is the main entry point for HCR execution. It:
/// 1. Validates the command against the profile's command policy
/// 2. Builds a sandboxed environment
/// 3. Manages process lifecycle (timeout, process group, cleanup)
/// 4. Returns a structured result
pub fn execute(
    profile: &HcrProfile,
    command_name: &str,
    params: &HashMap<String, String>,
    workspace_root: &Path,
) -> HcrExecResult {
    // Step 1: Command policy check
    let resolved =
        match CommandPolicy::check(command_name, params, profile, &workspace_root.to_path_buf()) {
            Ok(r) => r,
            Err(e) => {
                return HcrExecResult {
                    status: HcrStatus::Denied,
                    exit_code: -1,
                    timed_out: false,
                    stdout: String::new(),
                    stderr: format!("HCR command denied: {e}"),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    child_cleanup: CleanupStatus::Confirmed,
                    error_code: Some(e.error_code().into()),
                };
            }
        };

    // Step 2: Build environment
    let sandbox_home = process::resolve_home_dir(profile, workspace_root);
    let needs_sandbox_home_cleanup =
        sandbox_home.starts_with(workspace_root) && !sandbox_home.exists();

    if needs_sandbox_home_cleanup {
        let _ = std::fs::create_dir_all(&sandbox_home);
    }

    // Step 3: Build and configure the command
    let mut cmd = std::process::Command::new(&resolved.program);
    cmd.args(&resolved.args);
    cmd.current_dir(workspace_root);

    // Environment: clear + allowlist
    cmd.env_clear();
    for var_name in &profile.env_allowlist {
        match var_name.as_str() {
            "PATH" => {
                if let Some(v) = std::env::var_os("PATH") {
                    cmd.env("PATH", &v);
                }
            }
            "TMPDIR" => {
                if let Some(v) = std::env::var_os("TMPDIR") {
                    cmd.env("TMPDIR", &v);
                } else {
                    cmd.env("TMPDIR", std::env::temp_dir());
                }
            }
            "HOME" => {
                cmd.env("HOME", &sandbox_home);
            }
            "LANG" => {
                if let Some(v) = std::env::var_os("LANG") {
                    cmd.env("LANG", &v);
                }
            }
            "LC_ALL" => {
                if let Some(v) = std::env::var_os("LC_ALL") {
                    cmd.env("LC_ALL", &v);
                }
            }
            "LC_CTYPE" => {
                if let Some(v) = std::env::var_os("LC_CTYPE") {
                    cmd.env("LC_CTYPE", &v);
                }
            }
            _ => {
                if let Some(v) = std::env::var_os(var_name) {
                    cmd.env(var_name, &v);
                }
            }
        }
    }

    // Step 4: Set up process group for cleanup
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // Step 5: Optionally wrap with sandbox
    let backend = SandboxBackend::detect();
    let sandbox_config = SandboxConfig {
        workspace_root: workspace_root.to_path_buf(),
        home_dir: sandbox_home.clone(),
        real_home: process::dirs_fallback(),
        agent_core_repo: process::find_agent_core_repo(),
        network_policy: resolved.network.clone(),
    };

    let mut cmd = match sandbox::wrap_with_sandbox(&mut cmd, &sandbox_config, &backend) {
        Ok(c) => c,
        Err(e) => {
            if needs_sandbox_home_cleanup {
                let _ = std::fs::remove_dir_all(&sandbox_home);
            }
            return HcrExecResult {
                status: HcrStatus::Denied,
                exit_code: -1,
                timed_out: false,
                stdout: String::new(),
                stderr: format!("HCR sandbox unavailable: {e}"),
                stdout_truncated: false,
                stderr_truncated: false,
                child_cleanup: CleanupStatus::Confirmed,
                error_code: Some(e.error_code().into()),
            };
        }
    };

    // Configure stdio
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Step 6: Spawn the child
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if needs_sandbox_home_cleanup {
                let _ = std::fs::remove_dir_all(&sandbox_home);
            }
            return HcrExecResult {
                status: HcrStatus::Failed,
                exit_code: -1,
                timed_out: false,
                stdout: String::new(),
                stderr: format!("HCR spawn failed: {e}"),
                stdout_truncated: false,
                stderr_truncated: false,
                child_cleanup: CleanupStatus::Confirmed,
                error_code: Some(HcrError::SpawnFailed(e.to_string()).error_code().into()),
            };
        }
    };

    let max_output = profile.output_bytes_max;
    let timeout = Duration::from_millis(resolved.timeout_ms);

    // Step 7: Concurrent output draining
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));

    if let Some(pipe) = stdout_pipe {
        let b = Arc::clone(&out_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || process::drain_reader(pipe, b, d, max_output));
    }
    if let Some(pipe) = stderr_pipe {
        let b = Arc::clone(&err_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || process::drain_reader(pipe, b, d, max_output));
    }

    // Step 8: Wait with timeout and process group management
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
            done.store(true, Ordering::SeqCst);
            let _ = process::kill_process_tree(child_pid);
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    done.store(true, Ordering::SeqCst);

    // Step 9: Collect results
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

    let stdout_all = out_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr_all = err_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let stdout_truncated = stdout_all.len() > max_output;
    let stderr_truncated = stderr_all.len() > max_output;

    let stdout_str = process::trunc(&stdout_all, max_output);
    let stderr_str = process::trunc(&stderr_all, max_output);

    let status = if timed_out {
        HcrStatus::TimedOut
    } else if exit_code == 0 {
        HcrStatus::Succeeded
    } else {
        HcrStatus::Failed
    };

    let error_code = if timed_out {
        Some(HcrError::Timeout.error_code().into())
    } else if exit_code != 0 {
        Some(
            HcrError::SpawnFailed(format!("exit code {exit_code}"))
                .error_code()
                .into(),
        )
    } else {
        None
    };

    // Cleanup: remove temporary sandbox home if we created it
    let cleanup = if needs_sandbox_home_cleanup {
        match std::fs::remove_dir_all(&sandbox_home) {
            Ok(_) => CleanupStatus::Confirmed,
            Err(_e) => CleanupStatus::Failed,
        }
    } else {
        CleanupStatus::Confirmed
    };

    HcrExecResult {
        status,
        exit_code,
        timed_out,
        stdout: stdout_str,
        stderr: stderr_str,
        stdout_truncated,
        stderr_truncated,
        child_cleanup: cleanup,
        error_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn result_serializes_to_json() {
        let result = HcrExecResult {
            status: HcrStatus::Succeeded,
            exit_code: 0,
            timed_out: false,
            stdout: "output".into(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            child_cleanup: CleanupStatus::Confirmed,
            error_code: None,
        };
        let json = result.to_json();
        assert_eq!(json["ok"], true);
        assert_eq!(json["result"]["status"], "succeeded");
        assert_eq!(json["result"]["exit_code"], 0);
        assert_eq!(json["result"]["child_cleanup"], "confirmed");
    }

    #[test]
    fn failed_result_serializes_to_json() {
        let result = HcrExecResult {
            status: HcrStatus::Denied,
            exit_code: -1,
            timed_out: false,
            stdout: String::new(),
            stderr: "denied".into(),
            stdout_truncated: false,
            stderr_truncated: false,
            child_cleanup: CleanupStatus::Confirmed,
            error_code: Some("HCR_COMMAND_NOT_ALLOWED".into()),
        };
        let json = result.to_json();
        assert_eq!(json["ok"], false);
        assert_eq!(json["result"]["status"], "denied");
        assert_eq!(json["result"]["error_code"], "HCR_COMMAND_NOT_ALLOWED");
    }
}
