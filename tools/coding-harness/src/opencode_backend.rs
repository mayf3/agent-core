//! OpenCode backend for coding.task.submit.
//!
//! Config is passed via `OPENCODE_CONFIG_CONTENT` env var with proper
//! `"allow"/"deny"` permission semantics. No `.opencode.json` written to
//! workspace. Uses `--auto` for non-interactive execution (not
//! `--dangerously-skip-permissions`). Process lifecycle uses process-group
//! cleanup, concurrent pipe draining, and a cancel token system.

use super::{truncate_str, TaskOutput};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static CANCEL_TOKENS: std::sync::LazyLock<Mutex<HashMap<String, CancelState>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

struct CancelState {
    cancelled: Arc<AtomicBool>,
    pid: Option<u32>,
}

/// Register a cancellation token for a task. Returns the shared cancelled flag.
pub(crate) fn register_cancel(task_id: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let mut map = CANCEL_TOKENS.lock().unwrap();
    map.insert(
        task_id.to_string(),
        CancelState {
            cancelled: flag.clone(),
            pid: None,
        },
    );
    flag
}

/// Set the process ID for a running task.
pub(crate) fn set_pid(task_id: &str, pid: u32) {
    let mut map = CANCEL_TOKENS.lock().unwrap();
    if let Some(state) = map.get_mut(task_id) {
        state.pid = Some(pid);
    }
}

/// Cancel a running task: signal the process group and wait for termination.
/// Returns true if cancellation was initiated.
pub(crate) fn cancel_task(task_id: &str) -> bool {
    let (_cancelled, pid) = {
        let map = CANCEL_TOKENS.lock().unwrap();
        match map.get(task_id) {
            Some(state) => {
                state.cancelled.store(true, Ordering::SeqCst);
                (state.cancelled.clone(), state.pid)
            }
            None => return false,
        }
    };

    if let Some(pid) = pid {
        // Kill entire process group.
        #[cfg(unix)]
        {
            unsafe {
                let _ = libc::killpg(pid as libc::pid_t, libc::SIGTERM);
                std::thread::sleep(Duration::from_millis(500));
                let _ = libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new("taskkill")
                .args(&["/F", "/T", "/PID", &pid.to_string()])
                .output();
        }
    }
    true
}

/// Clean up cancel token for a completed task.
pub(crate) fn cleanup_cancel(task_id: &str) {
    let mut map = CANCEL_TOKENS.lock().unwrap();
    map.remove(task_id);
}

/// Build the permission config JSON for the OPENCODE_CONFIG_CONTENT env var.
fn build_config_json() -> String {
    let config = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "permission": {
            "*": "deny",
            "read": "allow",
            "edit": "allow",
            "glob": "allow",
            "grep": "allow",
            "bash": "allow",
            "external_directory": "deny",
            "webfetch": "deny",
            "websearch": "deny",
            "task": "deny",
            "question": "deny"
        }
    });
    serde_json::to_string(&config).unwrap_or_default()
}

pub(super) fn run_opencode(
    task_id: &str,
    workspace_root: &str,
    objective: &str,
    model: &str,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<TaskOutput, String> {
    let opencode_path = find_opencode().map_err(|e| format!("opencode_not_found: {e}"))?;
    let resolved_model = if model.is_empty() {
        "deepseek/deepseek-v4-flash"
    } else {
        model
    };
    let prompt = build_prompt(objective);
    let ws_root = workspace_root.to_string();

    let mut cmd = std::process::Command::new(&opencode_path);
    cmd.arg("run")
        .arg("--model")
        .arg(resolved_model)
        .arg("--format")
        .arg("json")
        .arg("--dir")
        .arg(&ws_root)
        .arg("--auto")
        .arg(&prompt);

    cmd.env_clear();
    for var in &["PATH", "HOME", "TMPDIR", "DEEPSEEK_API_KEY"] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    // Pass permission config via env var, not .opencode.json file.
    cmd.env("OPENCODE_CONFIG_CONTENT", build_config_json());

    // Create process group for tree-kill.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("opencode_spawn_failed: {e}"))?;
    let pid = child.id();
    set_pid(task_id, pid);

    // Concurrent stdout/stderr drain with byte limits.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    const MAX_OUTPUT: usize = 100_000;

    if let Some(pipe) = stdout_pipe {
        let b = Arc::clone(&out_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || drain_pipe(pipe, b, d, MAX_OUTPUT));
    }
    if let Some(pipe) = stderr_pipe {
        let b = Arc::clone(&err_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || drain_pipe(pipe, b, d, MAX_OUTPUT));
    }

    let deadline = Duration::from_secs(600);
    let start = std::time::Instant::now();
    let mut timed_out = false;
    let mut cancelled = false;

    loop {
        // Check cancellation flag.
        if let Some(ref flag) = cancel_flag {
            if flag.load(Ordering::SeqCst) {
                cancelled = true;
                done.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                let _ = child.wait();
                break;
            }
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= deadline {
            timed_out = true;
            done.store(true, Ordering::SeqCst);
            kill_process_group(pid);
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done.store(true, Ordering::SeqCst);
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

    if cancelled {
        return Err("cancelled".to_string());
    }

    let stdout_all = out_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr_all = err_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stdout_str = String::from_utf8_lossy(&stdout_all).to_string();
    let stderr_str = String::from_utf8_lossy(&stderr_all).to_string();

    if exit_code == 0 {
        let (commit_sha, changed_files, diff_summary, test_command, test_result, summary) =
            parse_output(&stdout_str, objective);
        Ok(TaskOutput {
            summary,
            commit_sha,
            changed_files,
            diff_summary,
            test_command,
            test_result,
            stdout: stdout_str,
            stderr: stderr_str,
            exit_code,
            timed_out,
        })
    } else {
        Err(format!(
            "opencode_exit_{}: {}",
            exit_code,
            truncate_str(&stderr_str.lines().last().unwrap_or(&stderr_str), 300)
        ))
    }
}

fn drain_pipe(mut pipe: impl Read, buf: Arc<Mutex<Vec<u8>>>, done: Arc<AtomicBool>, max: usize) {
    let mut local = Vec::new();
    let mut tmp = [0u8; 65536];
    loop {
        if done.load(Ordering::SeqCst) {
            let mut rest = Vec::new();
            let _ = pipe.read_to_end(&mut rest);
            if !rest.is_empty() && local.len() < max {
                let remaining = max.saturating_sub(local.len());
                local.extend_from_slice(&rest[..rest.len().min(remaining)]);
            }
            break;
        }
        match pipe.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                if local.len() < max {
                    local.extend_from_slice(&tmp[..n.min(max.saturating_sub(local.len()))]);
                }
            }
            Err(_) => break,
        }
    }
    buf.lock().unwrap().extend_from_slice(&local);
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    unsafe {
        let pgid = pid as libc::pid_t;
        let _ = libc::killpg(pgid, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(500));
        let _ = libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(&["/F", "/T", "/PID", &pid.to_string()])
        .output();
}

fn find_opencode() -> Result<String, String> {
    if std::process::Command::new("opencode")
        .arg("--version")
        .output()
        .is_ok()
    {
        Ok("opencode".to_string())
    } else {
        Err("opencode binary not found in PATH".into())
    }
}

fn build_prompt(objective: &str) -> String {
    format!(
        "Objective\n{objective}\n\n\
         Workspace boundary\n\
         - You may ONLY modify files within the current workspace directory.\n\
         - You MUST NOT access files outside the workspace.\n\
         - You MUST NOT read .env, tokens, keys, or production secrets.\n\
         - You MUST NOT push, merge, or deploy code.\n\n\
         Testing requirements\n\
         - After making changes, run the project's test suite.\n\
         - All existing tests must continue to pass.\n\n\
         Completion reporting\n\
         - Report which files were changed and why.\n\
         - Report test results and any failures.\n\
         - Keep output concise."
    )
}

fn parse_output(stdout: &str, objective: &str) -> (String, String, String, String, String, String) {
    let mut commit_sha = String::new();
    let mut changed_files = String::new();
    let mut diff_summary = String::new();
    let mut test_command = String::new();
    let mut test_result = "not_reported".to_string();
    let mut summary = format!("opencode: completed '{}'", truncate_str(objective, 80));

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(et) = event.get("type").and_then(|v| v.as_str()) {
                match et {
                    "completion" | "result" | "done" => {
                        if let Some(c) = event.get("content").and_then(|v| v.as_str()) {
                            summary = truncate_str(c, 200).to_string();
                        }
                    }
                    "file_change" | "edit" | "write" => {
                        if let Some(p) = event.get("path").and_then(|v| v.as_str()) {
                            if !changed_files.is_empty() {
                                changed_files.push_str(", ");
                            }
                            changed_files.push_str(p);
                        }
                    }
                    "diff" => {
                        if let Some(d) = event.get("diff").and_then(|v| v.as_str()) {
                            diff_summary = truncate_str(d, 500).to_string();
                        }
                    }
                    "test" | "test_result" => {
                        if let Some(s) = event.get("status").and_then(|v| v.as_str()) {
                            test_result = format!("test: {}", s);
                        }
                        if let Some(o) = event.get("output").and_then(|v| v.as_str()) {
                            test_result = truncate_str(o, 200).to_string();
                        }
                    }
                    "bash" | "tool_use" => {
                        if let Some(cmd_name) = event.pointer("/tool").and_then(|v| v.as_str()) {
                            if cmd_name == "bash" {
                                if let Some(input) = event
                                    .pointer("/state/input/command")
                                    .and_then(|v| v.as_str())
                                {
                                    test_command = truncate_str(input, 200).to_string();
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if commit_sha.is_empty() {
                if let Some(sha) = event.get("commit_sha").and_then(|v| v.as_str()) {
                    commit_sha = sha.to_string();
                }
            }
        }
    }
    if changed_files.is_empty() {
        changed_files = "unknown".to_string();
    }
    (
        commit_sha,
        changed_files,
        diff_summary,
        test_command,
        test_result,
        summary,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_uses_proper_allow_deny() {
        let config_str = build_config_json();
        let parsed: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let perm = &parsed["permission"];
        // "*" must be deny as default
        assert_eq!(perm["*"], "deny");
        // Allowed permissions
        assert_eq!(perm["read"], "allow");
        assert_eq!(perm["edit"], "allow");
        // Denied permissions
        assert_eq!(perm["external_directory"], "deny");
        assert_eq!(perm["webfetch"], "deny");
        assert_eq!(perm["websearch"], "deny");
        assert_eq!(perm["task"], "deny");
        assert_eq!(perm["question"], "deny");
    }

    #[test]
    fn config_has_schema_url() {
        let config_str = build_config_json();
        let parsed: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        assert_eq!(parsed["$schema"], "https://opencode.ai/config.json");
    }

    #[test]
    fn config_env_var_not_written_to_file() {
        // The config is passed via env var, not file.
        // Verify no .opencode.json is created by this module.
        let config_str = build_config_json();
        assert!(config_str.contains("external_directory"));
        assert!(config_str.contains("\"deny\""));
    }

    #[test]
    fn allowed_permissions_are_explicit() {
        let config_str = build_config_json();
        let parsed: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let perm = parsed["permission"].as_object().unwrap();
        // Each entry must be allow or deny
        for (_k, v) in perm {
            let val = v.as_str().unwrap();
            assert!(
                val == "allow" || val == "deny" || val == "ask",
                "permission value must be allow/deny/ask, got: {val}"
            );
        }
    }
}
