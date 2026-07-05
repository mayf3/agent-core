//! Artifact subprocess lifecycle.
//!
//! Reuses patterns from `tools/coding-harness/src/workspace.rs`:
//! direct argv execution, process group, concurrent stdout/stderr drain,
//! timeout → SIGTERM → SIGKILL cleanup.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::thread;

/// Result of executing an artifact subprocess.
pub(crate) struct ProcessOutput {
    pub stdout: String,
    #[allow(dead_code)]
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Errors from artifact execution.
#[derive(Debug)]
pub(crate) enum ProcessError {
    Timeout,
    IoError(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::Timeout => write!(f, "artifact execution timed out"),
            ProcessError::IoError(msg) => write!(f, "artifact I/O error: {msg}"),
        }
    }
}

/// Run an artifact binary with process-harness-v1 protocol.
///
/// 1. Writes JSON to stdin
/// 2. Concurrently drains stdout and stderr
/// 3. Polls for completion up to timeout
/// 4. Kills process group on timeout
/// 5. Returns captured output
pub(crate) fn run_artifact(
    artifact_path: &std::path::Path,
    stdin_json: &str,
    timeout: Duration,
    max_stdout: usize,
    max_stderr: usize,
) -> Result<ProcessOutput, ProcessError> {
    let mut child = Command::new(artifact_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| ProcessError::IoError(format!("spawn failed: {e}")))?;

    let child_pid = child.id() as i32;

    // Write stdin.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_json.as_bytes())
            .map_err(|e| ProcessError::IoError(format!("stdin write failed: {e}")))?;
        drop(stdin);
    }

    let done = Arc::new(AtomicBool::new(false));
    let stdout_handle = {
        let done = done.clone();
        let stdout = child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
        let max = max_stdout;
        thread::spawn(move || drain_pipe(stdout, max, done))
    };
    let stderr_handle = {
        let done = done.clone();
        let stderr = child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
        let max = max_stderr;
        thread::spawn(move || drain_pipe(stderr, max, done))
    };

    let deadline = Instant::now() + timeout;
    let exit_code: Option<i32> = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                done.store(true, Ordering::SeqCst);
                break status.code();
            }
            Ok(None) => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                done.store(true, Ordering::SeqCst);
                return Err(ProcessError::IoError(format!("wait failed: {e}")));
            }
        }
    };

    let exit_code = match exit_code {
        Some(code) => code,
        None => {
            done.store(true, Ordering::SeqCst);
            kill_process_group(child_pid);
            let _ = child.wait();
            return Err(ProcessError::Timeout);
        }
    };

    // Collect stdout/stderr.
    let stdout = stdout_handle
        .join()
        .map_err(|_| ProcessError::IoError("stdout thread join failed".to_string()))?
        .unwrap_or_default();
    let stderr = stderr_handle
        .join()
        .map_err(|_| ProcessError::IoError("stderr thread join failed".to_string()))?
        .unwrap_or_default();

    Ok(ProcessOutput {
        stdout,
        stderr,
        exit_code: Some(exit_code),
    })
}

/// Drain a pipe reader into a bounded string. Sets `done` to signal
/// completion and close the reader.
fn drain_pipe(
    reader: Option<Box<dyn Read + Send>>,
    max_bytes: usize,
    done: Arc<AtomicBool>,
) -> Option<String> {
    let reader = match reader {
        Some(r) => r,
        None => return None,
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut reader = reader.take((max_bytes + 1) as u64);
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > max_bytes {
                    let _ = reader.read_to_end(&mut Vec::new());
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if done.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    if done.load(Ordering::SeqCst) {
        let _ = reader.read_to_end(&mut buf);
    }
    String::from_utf8(buf).ok()
}

/// Kill a process group by PID of any member.
#[cfg(unix)]
fn kill_process_group(pid: i32) {
    use libc::{killpg, SIGKILL, SIGTERM};
    unsafe {
        let _ = killpg(pid, SIGTERM);
    }
    thread::sleep(Duration::from_millis(500));
    unsafe {
        let _ = killpg(pid, SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: i32) {
    // Non-Unix: child handle drop eventually cleans up.
}
