//! HCR process management helpers.
//!
//! Low-level functions for output draining, process group killing,
//! and output truncation. These are shared by the executor module.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::profile::HcrProfile;

/// Drain a pipe reader into a bounded buffer.
pub fn drain_reader(
    mut pipe: impl Read,
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
    max: usize,
) {
    let mut local = Vec::new();
    let mut tmp = [0u8; 65536];
    loop {
        if done.load(std::sync::atomic::Ordering::SeqCst) {
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

/// Kill a process tree by process group.
#[cfg(unix)]
pub fn kill_process_tree(pid: u32) {
    unsafe {
        let _ = libc::killpg(pid as libc::pid_t, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(500));
        let _ = libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub fn kill_process_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(&["/F", "/T", "/PID", &pid.to_string()])
        .output();
}

/// Truncate bytes to a string with `...` suffix if over max.
pub fn trunc(data: &[u8], max: usize) -> String {
    if data.len() <= max {
        String::from_utf8_lossy(data).to_string()
    } else {
        let mut s = String::from_utf8_lossy(&data[..max]).to_string();
        s.truncate(s.len().saturating_sub(3));
        s.push_str("...");
        s
    }
}

/// Resolve the HOME directory for the sandbox.
pub fn resolve_home_dir(profile: &HcrProfile, workspace_root: &Path) -> PathBuf {
    if let Some(ref home) = profile.sandbox_home {
        if home.is_absolute() {
            home.clone()
        } else {
            workspace_root.join(home)
        }
    } else {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        workspace_root.join(format!(".hcr-home-{ts}"))
    }
}

/// Find the real user home directory.
pub fn dirs_fallback() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
    } else {
        PathBuf::from("/nonexistent")
    }
}

/// Find the agent-core repository path.
pub fn find_agent_core_repo() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut cur = Some(cwd.as_path());
    while let Some(dir) = cur {
        if dir.join("Cargo.toml").exists() {
            if let Ok(content) = std::fs::read_to_string(dir.join("Cargo.toml")) {
                if content.contains("agent-core-kernel")
                    || content.contains("name = \"agent-core\"")
                {
                    return Some(dir.to_path_buf());
                }
            }
        }
        cur = dir.parent();
    }
    None
}
