//! Subprocess execution for `workspace.exec`.
//!
//! Security invariants:
//! - Uses `std::process::Command` (never `sh -c` or shell string).
//! - `program` and `args` are separate; no shell injection.
//! - `cwd` must be within the workspace root.
//! - Timeout enforced via external thread + process kill.
//! - Environment: only PATH, HOME, LANG/LC_*, TMPDIR, plus operator-configured vars.
//! - No capability tokens, .env, or IPC secrets passed through.

use crate::paths::resolve_path;
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Default max output bytes (256 KiB).
const DEFAULT_MAX_OUTPUT: usize = 262_144;

pub fn handle_exec(root: &Path, args: &Value, env_pass: &[String]) -> Value {
    let program = match args.get("program").and_then(Value::as_str) {
        Some(p) => p,
        None => return err("missing_program"),
    };
    if program.is_empty() {
        return err("empty_program");
    }

    let cmd_args: Vec<&str> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let relative_cwd = args
        .get("relative_cwd")
        .and_then(Value::as_str)
        .unwrap_or(".");

    let timeout_secs = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(300)
        .min(3600); // cap at 1 hour

    let max_output: usize = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_OUTPUT as u64)
        .min(DEFAULT_MAX_OUTPUT as u64 * 4) as usize; // cap at 1 MiB

    // Resolve cwd within workspace root.
    let cwd = match resolve_path(root, relative_cwd) {
        Ok(p) => p,
        Err(e) => return err(&format!("cwd_outside_workspace: {e}")),
    };
    if !cwd.is_dir() {
        return err("cwd_not_a_directory");
    }

    // Build the command.
    let mut cmd = Command::new(program);
    cmd.args(&cmd_args);
    cmd.current_dir(&cwd);

    // Set minimal safe environment.
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        cmd.env("HOME", home);
    }
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        cmd.env("TMPDIR", tmpdir);
    }
    // Pass LANG/LC_* for locale support.
    for (k, v) in std::env::vars() {
        if k.starts_with("LANG") || k.starts_with("LC_") {
            cmd.env(&k, v);
        }
    }
    // Pass operator-configured additional env vars.
    for var_name in env_pass {
        if let Some(v) = std::env::var_os(var_name) {
            cmd.env(var_name, v);
        }
    }

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Spawn and wait with timeout.
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let code = if e.kind() == std::io::ErrorKind::NotFound {
                "program_not_found"
            } else {
                "spawn_failed"
            };
            return err(&format!("{code}: {e}"));
        }
    };

    let deadline = Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    let timed_out;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                timed_out = false;
                break;
            }
            Ok(None) => {}
            Err(_) => {
                timed_out = false;
                break;
            }
        }
        if start.elapsed() >= deadline {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

    // Collect stdout/stderr with size limits.
    let stdout_all = read_output(&mut child, false);
    let stderr_all = read_output(&mut child, true);

    let stdout_truncated = stdout_all.len() > max_output;
    let stderr_truncated = stderr_all.len() > max_output;

    let stdout = truncate_utf8(&stdout_all, max_output);
    let stderr = truncate_utf8(&stderr_all, max_output);

    ok(json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "timed_out": timed_out,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "stdout_bytes": stdout_all.len(),
        "stderr_bytes": stderr_all.len(),
    }))
}

fn read_output(child: &mut std::process::Child, is_stderr: bool) -> Vec<u8> {
    use std::io::Read;
    let mut buf = Vec::new();
    let result = if is_stderr {
        child.stderr.take().map(|mut r| r.read_to_end(&mut buf))
    } else {
        child.stdout.take().map(|mut r| r.read_to_end(&mut buf))
    };
    match result {
        Some(Ok(_)) => buf,
        _ => buf,
    }
}

fn truncate_utf8(data: &[u8], max: usize) -> String {
    if data.len() <= max {
        String::from_utf8_lossy(data).to_string()
    } else {
        let truncated = &data[..max];
        let mut s = String::from_utf8_lossy(truncated).to_string();
        // Ensure it ends cleanly.
        s.truncate(s.len().saturating_sub(3));
        s.push_str("...");
        s
    }
}

fn ok(result: Value) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": true,
        "result": result,
    })
}

fn err(code: &str) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
    })
}
