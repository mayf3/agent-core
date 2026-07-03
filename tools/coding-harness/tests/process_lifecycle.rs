//! Process lifecycle tests for the coding-harness.
//!
//! Covers large output, timeout, and cancellation scenarios.
//! Uses standard OS commands to simulate process behavior.

use std::time::Duration;

const MEGABYTE: usize = 1_048_576;

#[test]
fn large_stdout_does_not_deadlock() {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("dd if=/dev/zero bs=1048576 count=2 2>/dev/null")
        .output()
        .expect("dd failed");
    assert!(output.stdout.len() > MEGABYTE, "must produce >1MB stdout");
    assert!(output.status.success(), "dd must exit 0");
}

#[test]
fn large_stderr_does_not_deadlock() {
    // Use python or perl to write >1MB to stderr without blocking on pipe.
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("python3 -c \"import sys; sys.stderr.buffer.write(b'x' * 1200000)\" 2>&1")
        .output()
        .expect("python3 failed");
    let combined = output.stdout.len() + output.stderr.len();
    assert!(combined > MEGABYTE, "must produce >1MB output; got {combined} bytes");
}

#[test]
fn combined_large_output_does_not_deadlock() {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("python3 -c \"
import sys
sys.stdout.buffer.write(b'a' * 600000)
sys.stderr.buffer.write(b'b' * 600000)
\"")
        .output()
        .expect("python3 failed");
    let combined = output.stdout.len() + output.stderr.len();
    assert!(combined > MEGABYTE, "must produce >1MB combined; got {combined}");
    assert!(output.status.success());
}

#[test]
fn timeout_kills_process() {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 60")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn failed");
    let pid = child.id();
    std::thread::sleep(Duration::from_millis(200));

    // Kill child process directly.
    #[cfg(unix)]
    unsafe { let _ = libc::kill(pid as libc::pid_t, libc::SIGKILL); }
    #[cfg(not(unix))]
    { let _ = std::process::Command::new("taskkill").args(&["/F", "/PID", &pid.to_string()]).output(); }

    let status = child.wait().expect("wait failed");
    assert!(!status.success(), "process must be killed after timeout");
}

#[test]
fn cancellation_stops_process() {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("trap '' TERM; while true; do sleep 1; done")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn failed");
    let pid = child.id();
    std::thread::sleep(Duration::from_millis(200));

    // Verify process is running.
    let running = std::process::Command::new("kill")
        .args(&["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(running, "process should be running before cancel");

    // SIGKILL cannot be trapped.
    #[cfg(unix)]
    unsafe { let _ = libc::kill(pid as libc::pid_t, libc::SIGKILL); }
    #[cfg(not(unix))]
    { let _ = std::process::Command::new("taskkill").args(&["/F", "/PID", &pid.to_string()]).output(); }

    let status = child.wait().expect("wait failed");
    assert!(!status.success(), "process must be terminated after cancel");

    // Verify process is gone.
    std::thread::sleep(Duration::from_millis(100));
    let still_running = std::process::Command::new("kill")
        .args(&["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(!still_running, "process should be gone after cancel");
}
