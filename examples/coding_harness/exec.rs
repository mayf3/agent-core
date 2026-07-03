//! Subprocess execution for coding.workspace.exec.
//! Concurrent stdout/stderr draining to prevent pipe deadlock.
//! Trust model: cwd constrained to workspace, but process has harness OS user permissions.

use crate::paths::resolve_path;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_MAX_OUTPUT: usize = 262_144;
const ABSOLUTE_MAX: usize = 1_048_576;

pub fn handle_exec(root: &Path, args: &Value) -> Value {
    let program = match args.get("program").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => p,
        _ => return err("missing_program"),
    };
    let cmd_args: Vec<&str> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let cwd_rel = args
        .get("relative_cwd")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let timeout_secs = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(300)
        .min(3600);
    let max_output = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_OUTPUT as u64)
        .min(ABSOLUTE_MAX as u64) as usize;

    let cwd = match resolve_path(root, cwd_rel) {
        Ok(p) => p,
        Err(e) => return err(&format!("cwd_error: {e}")),
    };
    if !cwd.is_dir() {
        return err("cwd_not_a_directory");
    }

    let mut cmd = Command::new(program);
    cmd.args(&cmd_args).current_dir(&cwd);
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

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return err(&format!(
                "{}: {e}",
                if e.kind() == std::io::ErrorKind::NotFound {
                    "program_not_found"
                } else {
                    "spawn_failed"
                }
            ))
        }
    };

    // ── Concurrent stdout/stderr draining ──
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_buf = Arc::new(std::sync::Mutex::new(Vec::new()));
    let err_buf = Arc::new(std::sync::Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));

    if let Some(pipe) = stdout_pipe {
        let b = Arc::clone(&out_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut r = pipe;
            let mut buf = [0u8; 65536];
            loop {
                if d.load(Ordering::SeqCst) {
                    let mut l = Vec::new();
                    let _ = r.read_to_end(&mut l);
                    if !l.is_empty() {
                        b.lock().unwrap().extend_from_slice(&l);
                    }
                    break;
                }
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        b.lock().unwrap().extend_from_slice(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        });
    }
    if let Some(pipe) = stderr_pipe {
        let b = Arc::clone(&err_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut r = pipe;
            let mut buf = [0u8; 65536];
            loop {
                if d.load(Ordering::SeqCst) {
                    let mut l = Vec::new();
                    let _ = r.read_to_end(&mut l);
                    if !l.is_empty() {
                        b.lock().unwrap().extend_from_slice(&l);
                    }
                    break;
                }
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        b.lock().unwrap().extend_from_slice(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // ── Wait with timeout ──
    let deadline = Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= deadline {
            timed_out = true;
            done.store(true, Ordering::SeqCst);
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done.store(true, Ordering::SeqCst);
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

    let stdout_all = out_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr_all = err_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let trunc = |data: &[u8], max: usize| -> String {
        if data.len() <= max {
            String::from_utf8_lossy(data).to_string()
        } else {
            let mut s = String::from_utf8_lossy(&data[..max]).to_string();
            s.truncate(s.len().saturating_sub(3));
            s.push_str("...");
            s
        }
    };

    ok(
        json!({"exit_code": exit_code, "stdout": trunc(&stdout_all, max_output), "stderr": trunc(&stderr_all, max_output),
        "timed_out": timed_out, "stdout_truncated": stdout_all.len() > max_output, "stderr_truncated": stderr_all.len() > max_output,
        "stdout_bytes": stdout_all.len(), "stderr_bytes": stderr_all.len()}),
    )
}

fn ok(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
fn err(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}
