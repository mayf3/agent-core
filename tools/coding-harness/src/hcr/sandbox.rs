//! HCR filesystem and network sandbox abstraction.
//!
//! Provides platform-specific filesystem and network isolation for HCR
//! child processes. macOS uses `sandbox-exec(1)` with a generated `.sb`
//! profile. Linux uses `bubblewrap` / `bwrap(1)`.
//!
//! If no supported backend is detected, all HCR sandbox operations
//! **fail closed** with `HCR_SANDBOX_UNAVAILABLE`.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use super::errors::HcrError;
use super::profile::NetworkPolicy;

/// Detected sandbox backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxBackend {
    /// macOS sandbox-exec(1)
    MacOSSandboxExec,
    /// Linux bubblewrap (bwrap)
    LinuxBubblewrap,
    /// No supported backend detected on this platform.
    Unavailable,
}

/// Configuration for sandbox execution.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Workspace root — child can read/write here.
    pub workspace_root: PathBuf,
    /// Sandbox home directory (not the real user home).
    pub home_dir: PathBuf,
    /// Real user home directory (blocked from child).
    pub real_home: PathBuf,
    /// Agent core repo path (blocked from child).
    pub agent_core_repo: Option<PathBuf>,
    /// Network policy for this execution.
    pub network_policy: NetworkPolicy,
}

impl SandboxBackend {
    /// Detect the available sandbox backend on this platform.
    ///
    /// Performs a functional test after binary detection to ensure the
    /// backend actually works (important on Apple Silicon where
    /// sandbox-exec exists but arm64e binaries cannot run inside it).
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        {
            // Check if sandbox-exec exists
            let exists = StdCommand::new("which")
                .arg("sandbox-exec")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
                || Path::new("/usr/bin/sandbox-exec").exists();

            if exists && sandbox_exec_works() {
                return SandboxBackend::MacOSSandboxExec;
            }
        }

        #[cfg(target_os = "linux")]
        {
            let output = StdCommand::new("which").arg("bwrap").output();
            if let Ok(out) = output {
                if out.status.success() {
                    return SandboxBackend::LinuxBubblewrap;
                }
            }
            if Path::new("/usr/bin/bwrap").exists() || Path::new("/usr/local/bin/bwrap").exists() {
                return SandboxBackend::LinuxBubblewrap;
            }
        }

        SandboxBackend::Unavailable
    }
}

/// Verify that sandbox-exec can actually execute a simple command.
///
/// On some macOS/Apple Silicon configurations, sandbox-exec exists but
/// cannot run arm64e system binaries due to Rosetta/provenance issues.
///
/// **H1 fix**: The original code created a piped stdin but never wrote
/// profile content to it, causing sandbox-exec to always fail with
/// "no version specified". Now writes a minimal permissive profile to
/// the child's stdin before waiting for the result.
#[cfg(target_os = "macos")]
fn sandbox_exec_works() -> bool {
    use std::io::Write;
    use std::time::{Duration, Instant};

    // Minimal permissive profile for the probe.
    let probe_profile = r#"(version 1)
(deny default)
(allow process-exec)
(allow file-read* (subpath "/"))
"#;

    let mut child = match StdCommand::new("sandbox-exec")
        .args(&["-f", "/dev/stdin", "--", "/bin/echo", "probe"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Write the profile to the child's stdin and close it.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(probe_profile.as_bytes());
        // Closing stdin by dropping lets sandbox-exec know the profile is
        // complete.
        drop(stdin);
    } else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }

    // Wait with a short timeout (2 seconds).
    let start = Instant::now();
    let timeout = Duration::from_secs(2);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return false; // Timeout → treat as unavailable.
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wrap a `std::process::Command` with sandbox execution.
///
/// On success, the returned `Command` is configured to run inside the
/// sandbox. The caller should then call `.spawn()` on it.
///
/// On failure (backend unavailable), returns `HcrError::SandboxUnavailable`.
pub fn wrap_with_sandbox(
    cmd: &mut StdCommand,
    config: &SandboxConfig,
    backend: &SandboxBackend,
) -> Result<StdCommand, HcrError> {
    match backend {
        SandboxBackend::MacOSSandboxExec => wrap_macos_sandbox_exec(cmd, config),
        SandboxBackend::LinuxBubblewrap => wrap_linux_bubblewrap(cmd, config),
        SandboxBackend::Unavailable => Err(HcrError::SandboxUnavailable),
    }
}

// ── macOS sandbox-exec ──

#[cfg(target_os = "macos")]
fn wrap_macos_sandbox_exec(
    cmd: &mut StdCommand,
    config: &SandboxConfig,
) -> Result<StdCommand, HcrError> {
    let profile = generate_macos_sb_profile(config);
    let profile_path = write_temp_sb_profile(&profile)?;

    // The sandbox execution becomes: sandbox-exec -f <profile.sb> -- <original_cmd>
    // We need to extract the original program and args
    let original_program = cmd.get_program().to_string_lossy().to_string();
    let original_args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let original_cwd = cmd.get_current_dir().map(|d| d.to_path_buf());
    let original_env: Vec<(String, String)> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                (
                    k.to_string_lossy().to_string(),
                    v.to_string_lossy().to_string(),
                )
            })
        })
        .collect();

    let mut sandbox_cmd = StdCommand::new("sandbox-exec");
    // Environment isolation: clear inherited vars, then set only the
    // allowlisted vars from the original command (env_clear is called
    // in the executor before wrap_with_sandbox is reached, so
    // original_env already contains only allowlisted entries).
    sandbox_cmd.env_clear();
    sandbox_cmd.arg("-f");
    sandbox_cmd.arg(profile_path.to_string_lossy().to_string());
    sandbox_cmd.arg("--");
    sandbox_cmd.arg(&original_program);
    sandbox_cmd.args(&original_args);
    if let Some(cwd) = original_cwd {
        sandbox_cmd.current_dir(cwd);
    }
    for (k, v) in original_env {
        sandbox_cmd.env(&k, &v);
    }

    Ok(sandbox_cmd)
}

#[cfg(not(target_os = "macos"))]
fn wrap_macos_sandbox_exec(
    _cmd: &mut StdCommand,
    _config: &SandboxConfig,
) -> Result<StdCommand, HcrError> {
    Err(HcrError::SandboxUnavailable)
}

/// Generate a macOS sandbox-exec profile (.sb) content.
///
/// ## Important: seatbelt rule ordering
///
/// Apple's Seatbelt sandbox uses a **last-matching-rule-wins** semantic.
/// Rules are evaluated top-to-bottom and the most recent match for a given
/// operation type determines the outcome. This means:
///
/// - Broad allow rules should come FIRST.
/// - Specific deny rules should come AFTER broad allows to override them.
/// - Specific allow exceptions (e.g. workspace access) should come AFTER
///   denies to re-allow paths that were denied.
///
/// ## H2 fix
///
/// Modern macOS (Sequoia / darwin 25) places the dyld shared cache on a
/// separate APFS volume (`/System/Volumes/Preboot/Cryptexes/OS/…`) that is
/// accessible only via the cryptex symlink under `/System/Cryptexes/OS`.
/// After symlink resolution this points to a **different physical volume**,
/// making explicit subpath allow-lists unreliable.  The only reliable
/// approach on this platform is `(allow file-read* (subpath "/"))` with
/// **selective deny rules** for sensitive paths.
///
/// ## H3 fix
///
/// The original code used `(local ip "127.0.0.1")` which (a) is
/// syntactically invalid without a port, and (b) matches the *source*
/// address of a connection — all outbound connections originate from a
/// local socket, so `(allow network* (local ip "127.0.0.1:*"))` would
/// actually allow *all* outbound traffic.  The correct operation is
/// `(remote ip "localhost:*")` which checks the *destination* address.
///
/// The `(deny network*)` must appear BEFORE the allow rule so that the
/// broad deny matches first; the more recent allow for localhost overrides
/// it only for loopback destinations.
fn generate_macos_sb_profile(config: &SandboxConfig) -> String {
    let ws = config.workspace_root.to_string_lossy();
    let home = config.home_dir.to_string_lossy();
    let real_home = config.real_home.to_string_lossy();
    let repo = config
        .agent_core_repo
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // ── Network policy ────────────────────────────────────────────────
    // Order: (deny network*) first, then (allow ...) for loopback.
    // Last matching rule wins, so the broader deny is overridden only for
    // destinations matching the allow.
    let net_policy = match config.network_policy {
        NetworkPolicy::Deny => {
            "(deny network*)".to_string()
        }
        NetworkPolicy::LoopbackOnly => {
            [
                "(deny network*)",
                "(allow network* (remote ip \"localhost:*\"))",
            ]
            .join("\n")
        }
    };

    // ── File-system policy ────────────────────────────────────────────
    // 1. Broad allow for all file-read on "/" (covers sealed SSV,
    //    cryptex-backed dyld cache, and firmlink destinations).
    // 2. Deny sensitive paths (MUST come after the broad allow).
    // 3. Re-allow workspace and home read-write (MUST come after denies
    //    so that a workspace inside a denied path can still be accessed).

    // Sensitive-path deny rules.
    let mut deny_rules = Vec::new();

    // Real user home (e.g. /Users/yanfenma).
    deny_rules.push(format!("(deny file-read* (subpath \"{real_home}\"))"));

    // Agent-core repository (deny even if it overlaps with real_home).
    if !repo.is_empty() {
        deny_rules.push(format!("(deny file-read* (subpath \"{repo}\"))"));
    }

    // SSH configuration.
    deny_rules.push("(deny file-read* (subpath \"/private/etc/ssh\"))".into());
    deny_rules.push("(deny file-read* (subpath \"/etc/ssh\"))".into());

    // Root home.
    deny_rules.push("(deny file-read* (subpath \"/var/root\"))".into());

    let deny_section = deny_rules.join("\n");

    format!(
        r#"(version 1)
; Default: deny everything
(deny default)

; Allow process execution and basic operations
(allow ipc-posix*)
(allow sysctl-read)
(allow process-fork)
(allow process-exec)
(allow signal)
(allow system-fsctl)

; H2: broad file-read allow covering sealed SSV, cryptex-backed dyld
; cache, and firmlink destinations.  Specific denies follow.
(allow file-read* (subpath "/"))

; H2: deny sensitive paths (last-match-wins: these override the broad
; allow above).
{deny_section}

; Allow read-write access to workspace root (overrides any preceding
; deny that would otherwise block a workspace inside a denied path).
(allow file-read* file-write* (subpath "{ws}"))

; Allow read-write access to sandbox home
(allow file-read* file-write* (subpath "{home}"))

; H3: network policy — deny all, then selectively allow localhost.
{net_policy}
"#
    )
}

/// Write a sandbox profile to a temporary file and return its path.
fn write_temp_sb_profile(content: &str) -> Result<PathBuf, HcrError> {
    let tmp_dir = std::env::temp_dir().join("hcr-sandbox-profiles");
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| HcrError::Internal(format!("failed to create sandbox profile dir: {e}")))?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let profile_path = tmp_dir.join(format!("hcr_{ts}.sb"));
    std::fs::write(&profile_path, content)
        .map_err(|e| HcrError::Internal(format!("failed to write sandbox profile: {e}")))?;

    Ok(profile_path)
}

// ── Linux bubblewrap ──

#[cfg(target_os = "linux")]
fn wrap_linux_bubblewrap(
    cmd: &mut StdCommand,
    config: &SandboxConfig,
) -> Result<StdCommand, HcrError> {
    let ws = config.workspace_root.to_string_lossy().to_string();
    let home = config.home_dir.to_string_lossy().to_string();

    let original_program = cmd.get_program().to_string_lossy().to_string();
    let original_args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let original_cwd = cmd.get_current_dir().map(|d| d.to_path_buf());
    let original_env: Vec<(String, String)> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                (
                    k.to_string_lossy().to_string(),
                    v.to_string_lossy().to_string(),
                )
            })
        })
        .collect();

    let mut bwrap = StdCommand::new("bwrap");
    // Environment isolation: clear inherited vars, then set only the
    // allowlisted vars from the original command.
    bwrap.env_clear();

    // Basic filesystem: unbind all, then bind specific paths
    bwrap.arg("--unshare-all");
    bwrap.arg("--new-session");

    // Required system paths (read-only)
    bwrap.arg("--ro-bind");
    bwrap.arg("/usr");
    bwrap.arg("/usr");
    bwrap.arg("--ro-bind");
    bwrap.arg("/lib");
    bwrap.arg("/lib");
    bwrap.arg("--ro-bind");
    bwrap.arg("/lib64");
    bwrap.arg("/lib64");
    bwrap.arg("--ro-bind");
    bwrap.arg("/bin");
    bwrap.arg("/bin");
    bwrap.arg("--ro-bind");
    bwrap.arg("/etc");
    bwrap.arg("/etc");
    bwrap.arg("--proc");
    bwrap.arg("/proc");
    bwrap.arg("--dev");
    bwrap.arg("/dev");
    bwrap.arg("--tmpfs");
    bwrap.arg("/tmp");

    // Workspace read-write
    bwrap.arg("--bind");
    bwrap.arg(&ws);
    bwrap.arg(&ws);

    // Home read-write
    bwrap.arg("--bind");
    bwrap.arg(&home);
    bwrap.arg(&home);

    // Network policy
    match config.network_policy {
        NetworkPolicy::Deny => {
            bwrap.arg("--unshare-net");
        }
        NetworkPolicy::LoopbackOnly => {
            // Allow loopback by not using --unshare-net
            // but restrict via iptables or rely on application-level
            // For now, bwrap doesn't support fine-grained network.
            // We document this limitation.
        }
    }

    // chdir to original cwd
    if let Some(cwd) = &original_cwd {
        bwrap.arg("--chdir");
        bwrap.arg(cwd.to_string_lossy().to_string());
    }

    // The program to run
    bwrap.arg(&original_program);
    bwrap.args(&original_args);

    // Set environment
    for (k, v) in original_env {
        bwrap.env(&k, &v);
    }

    Ok(bwrap)
}

#[cfg(not(target_os = "linux"))]
fn wrap_linux_bubblewrap(
    _cmd: &mut StdCommand,
    _config: &SandboxConfig,
) -> Result<StdCommand, HcrError> {
    Err(HcrError::SandboxUnavailable)
}

/// Return a human-readable description of the sandbox situation.
pub fn describe_sandbox_status() -> String {
    let backend = SandboxBackend::detect();
    match backend {
        SandboxBackend::MacOSSandboxExec => "macOS sandbox-exec available".into(),
        SandboxBackend::LinuxBubblewrap => "Linux bubblewrap (bwrap) available".into(),
        SandboxBackend::Unavailable => "no sandbox backend available — HCR will fail closed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detect_backend_never_panics() {
        // detect() should always return a valid value, never panic
        let backend = SandboxBackend::detect();
        match backend {
            SandboxBackend::MacOSSandboxExec
            | SandboxBackend::LinuxBubblewrap
            | SandboxBackend::Unavailable => {} // all valid
        }
    }

    #[test]
    fn generate_macos_profile_contains_workspace() {
        let config = SandboxConfig {
            workspace_root: PathBuf::from("/tmp/test-ws"),
            home_dir: PathBuf::from("/tmp/test-ws/.hcr-home"),
            real_home: PathBuf::from("/Users/testuser"),
            agent_core_repo: Some(PathBuf::from("/Users/testuser/project/agent-core")),
            network_policy: NetworkPolicy::Deny,
        };
        let profile = generate_macos_sb_profile(&config);

        // H2: broad file-read allow + specific denies
        assert!(profile.contains("(allow file-read* (subpath \"/\"))"));
        assert!(profile.contains("/tmp/test-ws"));
        assert!(profile.contains("/tmp/test-ws/.hcr-home"));
        assert!(profile.contains("/Users/testuser"));

        // Deny section for agent-core repo
        assert!(profile.contains("agent-core"));

        // H3: deny network* for Deny policy
        assert!(profile.contains("(deny network*)"));
        // No allow-localhost line for Deny
        assert!(!profile.contains("remote ip \"localhost:*\""));

        // Core operations
        assert!(profile.contains("(allow process-exec)"));
        assert!(profile.contains("(allow process-fork)"));

        // No debug noise
        assert!(!profile.contains("(debug deny)"));
    }

    #[test]
    fn generate_macos_profile_loopback() {
        let config = SandboxConfig {
            workspace_root: PathBuf::from("/tmp/test-ws"),
            home_dir: PathBuf::from("/tmp/test-ws/.hcr-home"),
            real_home: PathBuf::from("/Users/testuser"),
            agent_core_repo: None,
            network_policy: NetworkPolicy::LoopbackOnly,
        };
        let profile = generate_macos_sb_profile(&config);

        // H3: LoopbackOnly uses (deny network*) then (allow network* (remote ip ...))
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(allow network* (remote ip \"localhost:*\"))"));

        // H3 fix: no longer uses (local ip ...) or bare IP literals
        assert!(!profile.contains("(local ip"));
        assert!(!profile.contains("\"127.0.0.1\""));
        assert!(!profile.contains("\"::1\""));
    }

    #[test]
    fn unavailable_backend_fails_closed() {
        // Test that Unavailable returns SandboxUnavailable error
        let backend = SandboxBackend::Unavailable;
        let mut cmd = StdCommand::new("echo");
        let config = SandboxConfig {
            workspace_root: PathBuf::from("/tmp/ws"),
            home_dir: PathBuf::from("/tmp/ws/home"),
            real_home: PathBuf::from("/Users/user"),
            agent_core_repo: None,
            network_policy: NetworkPolicy::Deny,
        };
        let result = wrap_with_sandbox(&mut cmd, &config, &backend);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_code(), "HCR_SANDBOX_UNAVAILABLE");
    }

    #[test]
    fn describe_sandbox_never_empty() {
        let desc = describe_sandbox_status();
        assert!(!desc.is_empty());
    }
}
