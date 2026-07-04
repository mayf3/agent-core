use crate::config::WorkspacePermission;
use crate::paths::{resolve_path, resolve_path_unchecked, validate_relative};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const MAX_LIST: usize = 200;
const MAX_WRITE: usize = 2 * 1024 * 1024;
const MAX_READ: usize = 65536;
const DEFAULT_MAX_OUTPUT: usize = 262_144;
const ABSOLUTE_MAX: usize = 1_048_576;

static SEQ: AtomicU64 = AtomicU64::new(0);

fn ok_v(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
fn err_v(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}

pub fn handle_list(root: &Path, args: &Value) -> Value {
    let relative = match validate_relative(
        args.get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or("."),
    ) {
        Ok(r) => r,
        Err(e) => return err_v(&e.to_string()),
    };
    let dir_path = match resolve_path(root, relative) {
        Ok(p) => p,
        Err(e) => return err_v(&e.to_string()),
    };
    if !dir_path.is_dir() {
        return err_v("not_a_directory");
    }
    let mut entries = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir_path) {
        for entry in rd.flatten().take(MAX_LIST) {
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type().ok();
            let typ = if ft.map(|t| t.is_dir()).unwrap_or(false) {
                "dir"
            } else {
                "file"
            };
            let ep = entry.path();
            let rp = ep
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            entries.push(json!({"name": name, "type": typ, "relative_path": rp}));
        }
    }
    ok_v(json!({"entries": entries, "entry_count": entries.len()}))
}

pub fn handle_read(root: &Path, args: &Value) -> Value {
    let relative = match validate_relative(
        args.get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or(""),
    ) {
        Ok(r) => r,
        Err(e) => return err_v(&e.to_string()),
    };
    let path = match resolve_path(root, relative) {
        Ok(p) => p,
        Err(e) => return err_v(&e.to_string()),
    };
    if !path.is_file() {
        return err_v("not_a_file");
    }
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65536)
        .min(MAX_READ as u64) as usize;
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => return err_v(&format!("read_failed: {e}")),
    };
    let slice = &data[..data.len().min(max_bytes)];
    let content = match String::from_utf8(slice.to_vec()) {
        Ok(s) => s,
        Err(_) => return err_v("binary_file_not_supported"),
    };
    ok_v(
        json!({"content": content, "truncated": data.len() > max_bytes, "size_bytes": data.len(), "bytes_read": content.len()}),
    )
}

pub fn handle_write(root: &Path, args: &Value) -> Value {
    let relative = match validate_relative(
        args.get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or(""),
    ) {
        Ok(r) => r,
        Err(e) => return err_v(&e.to_string()),
    };
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("replace");
    if content.len() > MAX_WRITE {
        return err_v("content_exceeds_max_size");
    }
    let path = match resolve_path_unchecked(root, relative) {
        Ok(p) => p,
        Err(e) => return err_v(&e.to_string()),
    };
    if path.exists() && path.is_dir() {
        return err_v("is_a_directory");
    }
    if path.exists() && path.is_symlink() {
        return err_v("symlink_write_not_allowed");
    }
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return err_v(&format!("mkdir_failed: {e}"));
            }
        }
    }
    let bytes = match mode {
        "replace" => content.as_bytes().to_vec(),
        "append" => {
            let mut e = std::fs::read(&path).unwrap_or_default();
            e.extend_from_slice(content.as_bytes());
            e
        }
        other => return err_v(&format!("unknown_mode: {other}")),
    };
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!(".tmp_{}_{}", std::process::id(), seq));
    let wr = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = wr {
        let _ = std::fs::remove_file(&tmp);
        return err_v(&format!("write_failed: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return err_v(&format!("rename_failed: {e}"));
    }
    let mut h = Sha256::new();
    h.update(&bytes);
    ok_v(json!({"bytes_written": bytes.len(), "sha256": hex::encode(h.finalize()), "mode": mode}))
}

pub fn handle_exec(root: &Path, args: &Value, perm: &WorkspacePermission) -> Value {
    let shell_requested = args.get("shell").and_then(Value::as_bool).unwrap_or(false);
    if shell_requested && !perm.shell {
        return err_v("shell_not_permitted");
    }
    let program = match args.get("command").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => c,
        _ => match args.get("program").and_then(Value::as_str) {
            Some(p) if !p.is_empty() => p,
            _ => return err_v("missing_command"),
        },
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
        Err(e) => return err_v(&format!("cwd_error: {e}")),
    };
    if !cwd.is_dir() {
        return err_v("cwd_not_a_directory");
    }
    let mut cmd = if shell_requested {
        let mut sh = std::process::Command::new("sh");
        sh.arg("-c");
        let full_cmd = if cmd_args.is_empty() {
            program.to_string()
        } else {
            format!("{} {}", program, cmd_args.join(" "))
        };
        sh.arg(full_cmd);
        sh
    } else {
        let mut c = std::process::Command::new(program);
        c.args(&cmd_args);
        c
    };
    cmd.current_dir(&cwd);
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
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return err_v(if e.kind() == std::io::ErrorKind::NotFound {
                "program_not_found"
            } else {
                "spawn_failed"
            })
        }
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let err_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_o = max_output;
    let max_e = max_output;

    if let Some(pipe) = stdout_pipe {
        let b = std::sync::Arc::clone(&out_buf);
        let d = std::sync::Arc::clone(&done);
        std::thread::spawn(move || drain_reader(pipe, b, d, max_o));
    }
    if let Some(pipe) = stderr_pipe {
        let b = std::sync::Arc::clone(&err_buf);
        let d = std::sync::Arc::clone(&done);
        std::thread::spawn(move || drain_reader(pipe, b, d, max_e));
    }

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
            let _ = kill_process_tree(child.id());
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done.store(true, Ordering::SeqCst);
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);

    let stdout_all = out_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr_all = err_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    ok_v(
        json!({"exit_code": exit_code, "stdout": trunc(&stdout_all, max_output), "stderr": trunc(&stderr_all, max_output),
        "timed_out": timed_out, "stdout_truncated": stdout_all.len() > max_output, "stderr_truncated": stderr_all.len() > max_output,
        "stdout_bytes": stdout_all.len(), "stderr_bytes": stderr_all.len()}),
    )
}

fn drain_reader(
    mut pipe: impl Read,
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    max: usize,
) {
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
                    let remaining = max.saturating_sub(local.len());
                    local.extend_from_slice(&tmp[..n.min(remaining)]);
                }
            }
            Err(_) => break,
        }
    }
    buf.lock().unwrap().extend_from_slice(&local);
}

#[cfg(unix)]
fn kill_process_tree(pid: u32) {
    // Kill the process group. We use raw syscall wrappers via libc.
    // First try SIGTERM for graceful shutdown, then SIGKILL.
    unsafe {
        let _ = libc::killpg(pid as libc::pid_t, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(500));
        let _ = libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(&["/F", "/T", "/PID", &pid.to_string()])
        .output();
}

fn trunc(data: &[u8], max: usize) -> String {
    if data.len() <= max {
        String::from_utf8_lossy(data).to_string()
    } else {
        let mut s = String::from_utf8_lossy(&data[..max]).to_string();
        s.truncate(s.len().saturating_sub(3));
        s.push_str("...");
        s
    }
}
