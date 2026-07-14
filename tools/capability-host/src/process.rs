//! Bounded artifact subprocess lifecycle.

use crate::artifact::ResolvedArtifact;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub struct ProcessOutput {
    pub stdout: String,
    #[allow(dead_code)]
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug)]
pub enum ProcessError {
    Timeout,
    IoError(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "artifact execution timed out"),
            Self::IoError(message) => write!(f, "artifact I/O error: {message}"),
        }
    }
}

/// Execute only the descriptor-backed artifact that was verified from CAS.
/// The child receives no inherited environment or descriptors containing Host
/// tokens. Its process group is killed on every exit path so descendants cannot
/// keep output pipes alive after the direct child exits.
pub fn run_artifact(
    artifact: &ResolvedArtifact,
    stdin_json: &str,
    timeout: Duration,
    max_stdout: usize,
    max_stderr: usize,
) -> Result<ProcessOutput, ProcessError> {
    let executable = artifact
        .verified_execution_path()
        .map_err(|error| ProcessError::IoError(format!("artifact verification failed: {error}")))?;
    let mut child = Command::new(executable)
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| ProcessError::IoError(format!("spawn failed: {error}")))?;
    let child_pid = child.id() as i32;

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(stdin_json.as_bytes()) {
            terminate_and_reap(&mut child, child_pid, true);
            return Err(ProcessError::IoError(format!(
                "stdin write failed: {error}"
            )));
        }
    }

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_and_reap(&mut child, child_pid, true);
            return Err(ProcessError::IoError("stdout pipe missing".into()));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_and_reap(&mut child, child_pid, true);
            return Err(ProcessError::IoError("stderr pipe missing".into()));
        }
    };
    let stdout_handle = thread::spawn(move || drain_bounded(stdout, max_stdout));
    let stderr_handle = thread::spawn(move || drain_bounded(stderr, max_stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if Instant::now() >= deadline {
            terminate_and_reap(&mut child, child_pid, true);
            let _ = join_output(stdout_handle, stderr_handle);
            return Err(ProcessError::Timeout);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                terminate_and_reap(&mut child, child_pid, true);
                let _ = join_output(stdout_handle, stderr_handle);
                return Err(ProcessError::IoError(format!("wait failed: {error}")));
            }
        }
    };

    // The direct child is reaped, but a descendant may still hold stdout or
    // stderr. Kill the entire group before joining either drain thread.
    kill_group(child_pid, false);
    let (stdout, stderr) = join_output(stdout_handle, stderr_handle)?;
    Ok(ProcessOutput {
        stdout,
        stderr,
        exit_code: status.code(),
    })
}

fn drain_bounded(mut reader: impl Read, limit: usize) -> String {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut chunk = [0u8; 4096];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if bytes.len() <= limit {
            let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..read.min(remaining)]);
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

fn join_output(
    stdout: thread::JoinHandle<String>,
    stderr: thread::JoinHandle<String>,
) -> Result<(String, String), ProcessError> {
    let stdout = stdout
        .join()
        .map_err(|_| ProcessError::IoError("stdout drain failed".into()))?;
    let stderr = stderr
        .join()
        .map_err(|_| ProcessError::IoError("stderr drain failed".into()))?;
    Ok((stdout, stderr))
}

fn terminate_and_reap(child: &mut Child, pid: i32, graceful: bool) {
    kill_group(pid, graceful);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn kill_group(pid: i32, graceful: bool) {
    unsafe {
        if graceful {
            let _ = libc::killpg(pid, libc::SIGTERM);
            thread::sleep(Duration::from_millis(200));
        }
        let _ = libc::killpg(pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: i32, _graceful: bool) {}
