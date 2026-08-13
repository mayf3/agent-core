//! Execute one complete shell command in a real absolute cwd.
//!
//! The model-facing API is deliberately singular: `{cwd, command}`. On Linux,
//! the shell always runs inside bubblewrap with only system runtime paths and
//! the configured allowed roots mounted. There is no direct-program fallback.

use crate::filesystem::AllowedRoots;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
const DEFAULT_MAX_OUTPUT: usize = 16 * 1024;
const MAX_OUTPUT: usize = 24 * 1024;
const PRIVATE_HOME: &str = "/run/execution-harness-home";

#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    total: usize,
}

pub fn sandbox_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/usr/bin/bwrap").is_file() || Path::new("/usr/local/bin/bwrap").is_file()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn resolve_cwd(roots: &AllowedRoots, arguments: &Value) -> Result<PathBuf, String> {
    let cwd = arguments
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "missing_cwd".to_string())?;
    let cwd = roots
        .resolve_existing(cwd)
        .map_err(|error| match error.as_str() {
            "path_not_found" => "cwd_not_found".to_string(),
            "path_not_absolute" => "cwd_not_absolute".to_string(),
            other => other.to_string(),
        })?;
    if !cwd.is_dir() {
        return Err("cwd_not_directory".into());
    }
    Ok(cwd)
}

fn allowed_environment(command: &mut Command) {
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command.env("HOME", PRIVATE_HOME);
    command.env("TMPDIR", "/tmp");
    for (name, value) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("LANG") || name.to_string_lossy().starts_with("LC_") {
            command.env(name, value);
        }
    }
}

#[cfg(target_os = "linux")]
fn append_parent_dirs(command: &mut Command, path: &Path) {
    let mut parents: Vec<_> = path
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .filter(|parent| *parent != Path::new("/"))
        .collect();
    parents.reverse();
    for parent in parents {
        command.arg("--dir").arg(parent);
    }
}

#[cfg(target_os = "linux")]
fn sandboxed_shell(roots: &AllowedRoots, cwd: &Path, shell_line: &str) -> Result<Command, String> {
    let bwrap = if Path::new("/usr/bin/bwrap").is_file() {
        "/usr/bin/bwrap"
    } else if Path::new("/usr/local/bin/bwrap").is_file() {
        "/usr/local/bin/bwrap"
    } else {
        return Err("sandbox_unavailable".into());
    };
    let mut command = Command::new(bwrap);
    allowed_environment(&mut command);
    command.arg("--unshare-all");
    for path in ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"] {
        if Path::new(path).exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    command
        .arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev")
        .arg("--tmpfs")
        .arg("/tmp");

    for root in roots.as_slice() {
        append_parent_dirs(&mut command, root);
        command.arg("--bind").arg(root).arg(root);
    }

    command
        .arg("--dir")
        .arg("/run")
        .arg("--tmpfs")
        .arg(PRIVATE_HOME)
        .arg("--dir")
        .arg("/opt");

    let host_rustup = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|home| format!("{home}/.rustup"))
            .unwrap_or_default()
    });
    if !host_rustup.is_empty() && Path::new(&host_rustup).is_dir() {
        command
            .arg("--ro-bind")
            .arg(&host_rustup)
            .arg("/opt/rustup");
        command.env("RUSTUP_HOME", "/opt/rustup");
    }

    let host_cargo = std::env::var("CARGO_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|home| format!("{home}/.cargo"))
            .unwrap_or_default()
    });
    command.arg("--dir").arg("/opt/cargo");
    if !host_cargo.is_empty() && Path::new(&host_cargo).is_dir() {
        for child in ["bin", "registry", "git", ".crates.toml", ".crates2.json"] {
            let source = Path::new(&host_cargo).join(child);
            if source.exists() {
                let target = Path::new("/opt/cargo").join(child);
                command.arg("--ro-bind").arg(source).arg(target);
            }
        }
        command.env("CARGO_HOME", "/opt/cargo");
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("/opt/cargo/bin:{path}"));
    }

    command
        .arg("--unshare-net")
        .arg("--chdir")
        .arg(cwd)
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(shell_line);
    Ok(command)
}

#[cfg(not(target_os = "linux"))]
fn sandboxed_shell(_roots: &AllowedRoots, cwd: &Path, shell_line: &str) -> Result<Command, String> {
    // Non-Linux is used only for local unit tests. Production startup on Linux
    // fails closed when bubblewrap is unavailable.
    let mut command = Command::new("/bin/sh");
    allowed_environment(&mut command);
    command.current_dir(cwd).arg("-c").arg(shell_line);
    Ok(command)
}

fn prepare_process(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}

fn drain<R: Read + Send + 'static>(mut reader: R, output: Arc<Mutex<Captured>>, cap: usize) {
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let mut output = output.lock().unwrap();
                output.total = output.total.saturating_add(count);
                if output.bytes.len() < cap {
                    let take = (cap - output.bytes.len()).min(count);
                    output.bytes.extend_from_slice(&chunk[..take]);
                }
            }
        }
    }
}

pub fn execute(roots: &AllowedRoots, arguments: &Value) -> Result<Value, String> {
    let cwd = resolve_cwd(roots, arguments)?;
    let shell_line = arguments
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .ok_or_else(|| "missing_command".to_string())?;
    let timeout_secs = arguments
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);
    let max_output = arguments
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_OUTPUT as u64)
        .clamp(1, MAX_OUTPUT as u64) as usize;

    let mut command = sandboxed_shell(roots, &cwd, shell_line)?;
    prepare_process(&mut command);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "sandbox_unavailable".to_string()
        } else {
            "spawn_failed".to_string()
        }
    })?;

    let stdout = Arc::new(Mutex::new(Captured::default()));
    let stderr = Arc::new(Mutex::new(Captured::default()));
    let mut readers = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        let output = Arc::clone(&stdout);
        readers.push(std::thread::spawn(move || drain(pipe, output, max_output)));
    }
    if let Some(pipe) = child.stderr.take() {
        let output = Arc::clone(&stderr);
        readers.push(std::thread::spawn(move || drain(pipe, output, max_output)));
    }

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if started.elapsed() >= Duration::from_secs(timeout_secs) {
            timed_out = true;
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(200));
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if status.is_none() {
        let _ = child.wait();
    }
    for reader in readers {
        let _ = reader.join();
    }

    let stdout = stdout.lock().unwrap();
    let stderr = stderr.lock().unwrap();
    Ok(json!({
        "cwd": cwd.to_string_lossy(),
        "exit_code": status.and_then(|status| status.code()).unwrap_or(-1),
        "stdout": String::from_utf8_lossy(&stdout.bytes),
        "stderr": String::from_utf8_lossy(&stderr.bytes),
        "timed_out": timed_out,
        "stdout_bytes": stdout.total,
        "stderr_bytes": stderr.total,
        "stdout_truncated": stdout.total > stdout.bytes.len(),
        "stderr_truncated": stderr.total > stderr.bytes.len(),
        "sandboxed": cfg!(target_os = "linux"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "execution-harness-exec-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn complete_shell_line_supports_spaces_pipe_and_and() {
        let root = temp_root("shell");
        std::fs::write(root.join("input"), "alpha\nbeta\n").unwrap();
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let output = execute(
            &roots,
            &json!({
                "cwd": root,
                "command": "grep beta input | tr a-z A-Z && printf ' done'",
                "timeout_seconds": 10
            }),
        )
        .unwrap();
        assert_eq!(output["exit_code"], 0);
        assert_eq!(output["stdout"], "BETA\n done");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_and_rg_run_as_normal_shell_commands() {
        let root = temp_root("git-rg");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let output = execute(
            &roots,
            &json!({
                "cwd": root,
                "command": "git init -q && printf 'needle in a haystack\n' > note.txt && rg needle note.txt && git status --short",
            }),
        )
        .unwrap();
        assert_eq!(output["exit_code"], 0, "stderr={}", output["stderr"]);
        assert!(output["stdout"]
            .as_str()
            .unwrap()
            .contains("needle in a haystack"));
        assert!(output["stdout"].as_str().unwrap().contains("?? note.txt"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stdout_stderr_and_nonzero_exit_are_receipt_data() {
        let root = temp_root("nonzero");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let output = execute(
            &roots,
            &json!({"cwd": root, "command": "echo out; echo err >&2; exit 7"}),
        )
        .unwrap();
        assert_eq!(output["exit_code"], 7);
        assert_eq!(output["stdout"], "out\n");
        assert_eq!(output["stderr"], "err\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let root = temp_root("timeout");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let started = Instant::now();
        let output = execute(
            &roots,
            &json!({"cwd": root, "command": "sleep 60", "timeout_seconds": 1}),
        )
        .unwrap();
        assert_eq!(output["timed_out"], true);
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cwd_must_exist_and_be_inside_an_allowed_root() {
        let root = temp_root("cwd");
        let outside = temp_root("outside");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        assert_eq!(
            execute(
                &roots,
                &json!({"cwd": root.join("missing"), "command": "true"})
            )
            .unwrap_err(),
            "cwd_not_found"
        );
        assert_eq!(
            execute(&roots, &json!({"cwd": outside, "command": "true"})).unwrap_err(),
            "path_escape"
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_cannot_write_outside_allowed_roots() {
        let root = temp_root("sandbox-root");
        let outside = temp_root("sandbox-outside");
        let escaped = outside.join("escaped.txt");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let output = execute(
            &roots,
            &json!({
                "cwd": root,
                "command": format!("printf escaped > '{}'", escaped.display()),
            }),
        )
        .unwrap();
        assert_ne!(output["exit_code"], 0);
        assert!(!escaped.exists());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }
}
