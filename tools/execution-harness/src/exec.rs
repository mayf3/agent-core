//! Command execution inside a fenced workspace.
//!
//! Mirrors the proven execution pattern of the Coding Harness
//! (`src/workspace.rs handle_exec`): environment is cleared except
//! PATH/HOME/TMPDIR/LANG/LC_, the working directory is inside the
//! workspace fence, and every command has a timeout and output caps.
//! The process group is killed on timeout so children cannot outlive
//! the deadline.

use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
const DEFAULT_MAX_OUTPUT: usize = 32 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;

fn workspace_dir(root: &Path, workspace_id: &str) -> Result<std::path::PathBuf, String> {
    if workspace_id.is_empty()
        || !workspace_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("invalid_workspace_id".into());
    }
    let dir = root.join(workspace_id);
    std::fs::create_dir_all(&dir).map_err(|_| "workspace_create_failed".to_string())?;
    let canonical = dir.canonicalize().map_err(|_| "workspace_unreadable".to_string())?;
    if !canonical.starts_with(root) {
        return Err("path_escape".into());
    }
    Ok(canonical)
}

fn resolve_cwd(root: &Path, workspace_id: &str, relative_cwd: &str) -> Result<std::path::PathBuf, String> {
    use std::path::Component;
    let ws = workspace_dir(root, workspace_id)?;
    let rel = Path::new(relative_cwd);
    if rel.is_absolute() {
        return Err("path_escape".into());
    }
    for comp in rel.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err("path_escape".into()),
        }
    }
    let joined = ws.join(rel);
    let canonical = joined.canonicalize().unwrap_or(joined);
    if !canonical.starts_with(&ws) {
        return Err("path_escape".into());
    }
    if !canonical.is_dir() {
        return Err("cwd_not_a_directory".into());
    }
    Ok(canonical)
}

pub fn execute(root: &Path, arguments: &Value) -> Result<Value, String> {
    let workspace_id = arguments
        .get("workspace_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_workspace_id".to_string())?;
    let program = match arguments.get("command").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => c,
        _ => match arguments.get("program").and_then(Value::as_str) {
            Some(p) if !p.is_empty() => p,
            _ => return Err("missing_command".into()),
        },
    };
    let cmd_args: Vec<&str> = arguments
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let shell_requested = arguments
        .get("shell")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let relative_cwd = arguments
        .get("relative_cwd")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let timeout_secs = arguments
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .min(MAX_TIMEOUT_SECS)
        .max(1);
    let max_output = arguments
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_OUTPUT as u64)
        .min(MAX_OUTPUT as u64) as usize;

    let cwd = resolve_cwd(root, workspace_id, relative_cwd)?;

    // Build the command (direct exec or `sh -c` when shell is requested).
    let mut cmd = if shell_requested {
        let mut c = Command::new("sh");
        let full = if cmd_args.is_empty() {
            program.to_string()
        } else {
            format!("{} {}", program, cmd_args.join(" "))
        };
        c.arg("-c").arg(full);
        c
    } else {
        let mut c = Command::new(program);
        c.args(&cmd_args);
        c
    };

    cmd.current_dir(&cwd);
    // Env hygiene: clear everything except PATH/HOME/TMPDIR and locale.
    cmd.env_clear();
    for var in &["PATH", "HOME", "TMPDIR"] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    for (k, v) in std::env::vars() {
        if k.starts_with("LANG") || k.starts_with("LC_") {
            cmd.env(&k, v);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Run in its own process group so a timeout can kill the whole tree.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "program_not_found".to_string()
        } else {
            "spawn_failed".to_string()
        }
    })?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let err_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut drainers = Vec::new();
    if let Some(pipe) = stdout_pipe {
        let b = std::sync::Arc::clone(&out_buf);
        drainers.push(std::thread::spawn(move || {
            let mut reader = pipe;
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut guard = b.lock().unwrap();
                        if guard.len() < max_output {
                            let take = (max_output - guard.len()).min(n);
                            guard.extend_from_slice(&chunk[..take]);
                        }
                    }
                }
            }
        }));
    }
    if let Some(pipe) = stderr_pipe {
        let b = std::sync::Arc::clone(&err_buf);
        drainers.push(std::thread::spawn(move || {
            let mut reader = pipe;
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut guard = b.lock().unwrap();
                        if guard.len() < max_output {
                            let take = (max_output - guard.len()).min(n);
                            guard.extend_from_slice(&chunk[..take]);
                        }
                    }
                }
            }
        }));
    }

    let deadline = Duration::from_secs(timeout_secs);
    let start = Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= deadline {
            timed_out = true;
            // Kill the whole process group.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(200));
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

    // Wait for the output drain threads to finish reading the pipes before
    // reading the buffers (avoids a race where output is still in flight).
    for d in drainers {
        let _ = d.join();
    }

    let stdout_all = out_buf.lock().unwrap().clone();
    let stderr_all = err_buf.lock().unwrap().clone();
    let stdout = String::from_utf8_lossy(&stdout_all).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_all).into_owned();

    Ok(json!({
        "workspace_id": workspace_id,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "timed_out": timed_out,
        "stdout_bytes": stdout_all.len(),
        "stderr_bytes": stderr_all.len(),
        "stdout_truncated": stdout_all.len() >= max_output,
        "stderr_truncated": stderr_all.len() >= max_output,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "exec-harness-exec-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn simple_command_returns_output() {
        let root = tmp_root("simple");
        let r = execute(
            &root,
            &json!({
                "workspace_id": "ws",
                "command": "sh",
                "args": ["-c", "echo hello; echo err >&2; exit 3"],
                "timeout_seconds": 10,
            }),
        )
        .unwrap();
        assert_eq!(r["exit_code"], 3);
        assert_eq!(r["stdout"], "hello\n");
        assert_eq!(r["stderr"], "err\n");
        assert_eq!(r["timed_out"], false);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_program_rejected() {
        let root = tmp_root("missing");
        let r = execute(&root, &json!({"workspace_id": "ws"}));
        assert_eq!(r.unwrap_err(), "missing_command");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn timeout_kills_process_tree() {
        let root = tmp_root("timeout");
        let start = Instant::now();
        let r = execute(
            &root,
            &json!({
                "workspace_id": "ws",
                "command": "sh",
                "args": ["-c", "sleep 60"],
                "timeout_seconds": 2,
            }),
        )
        .unwrap();
        assert!(r["timed_out"] == true, "must time out");
        assert!(start.elapsed() < Duration::from_secs(10), "must not wait 60s");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cwd_escape_rejected() {
        let root = tmp_root("cwd");
        let r = execute(
            &root,
            &json!({
                "workspace_id": "ws",
                "command": "pwd",
                "relative_cwd": "../../..",
                "timeout_seconds": 5,
            }),
        );
        assert_eq!(r.unwrap_err(), "path_escape");
        let _ = std::fs::remove_dir_all(&root);
    }
}
