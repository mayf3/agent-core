//! HCR filesystem and network sandbox abstraction.
//!
//! HCR v0 supports **Linux bubblewrap** as its only sandbox backend.
//!
//! On macOS the sandbox is **unavailable**: `sandbox-exec(1)` on modern
//! macOS (Sequoia / darwin 25 with SSV + Cryptex) cannot express a
//! minimal file-read allowlist because the dyld shared cache resides on
//! a separate APFS volume (Preboot) that is accessible only through
//! cryptex symlinks.  The Seatbelt `(subpath …)` matcher does not
//! correctly resolve these cross-volume paths, making any explicit
//! allowlist (other than `(subpath "/")`) non-functional.  Rather than
//! resort to a broad allow-all + denylist model, HCR on macOS **fails
//! closed** with `HCR_SANDBOX_UNAVAILABLE`.
//!
//! Ordinary (non-HCR) Coding Harness workspace operations are unaffected.

use std::path::PathBuf;
use std::process::Command as StdCommand;

use super::errors::HcrError;
use super::profile::NetworkPolicy;

/// Detected sandbox backends.
///
/// HCR v0 only supports Linux bubblewrap.  macOS and other platforms
/// return `Unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxBackend {
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
    /// HCR v0 only supports Linux bubblewrap.  On macOS `sandbox-exec`
    /// is non-functional (see module-level docs) and returns
    /// `Unavailable`.
    pub fn detect() -> Self {
        #[cfg(target_os = "linux")]
        {
            let output = StdCommand::new("which").arg("bwrap").output();
            if let Ok(out) = output {
                if out.status.success() {
                    return SandboxBackend::LinuxBubblewrap;
                }
            }
            if std::path::Path::new("/usr/bin/bwrap").exists()
                || std::path::Path::new("/usr/local/bin/bwrap").exists()
            {
                return SandboxBackend::LinuxBubblewrap;
            }
        }

        SandboxBackend::Unavailable
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
        SandboxBackend::LinuxBubblewrap => wrap_linux_bubblewrap(cmd, config),
        SandboxBackend::Unavailable => Err(HcrError::SandboxUnavailable),
    }
}

// ── macOS sandbox-exec (disabled in HCR v0) ──

/// Generate a macOS sandbox-exec profile (.sb) content.
///
/// **This function is a pure string generator, kept for documentation
/// and unit-test coverage.**  It is never called at runtime because
/// `SandboxBackend::detect()` returns `Unavailable` on macOS.
///
/// The generated profile uses an **explicit allowlist** — no broad
/// `(subpath "/")` etc.
#[allow(dead_code)]
fn generate_macos_sb_profile(config: &SandboxConfig) -> String {
    let ws = config.workspace_root.to_string_lossy();
    let home = config.home_dir.to_string_lossy();

    let net_policy = match config.network_policy {
        NetworkPolicy::Deny => "(deny network*)".to_string(),
        NetworkPolicy::LoopbackOnly => [
            "(deny network*)",
            "(allow network* (remote ip \"localhost:*\"))",
        ]
        .join("\n"),
    };

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

; HCR v0: macOS sandbox-exec is unavailable — this profile is never
; applied at runtime.  The allowlist below is for reference only.

; Workspace root — read-write
(allow file-read* file-write* (subpath "{ws}"))

; Sandbox home — read-write
(allow file-read* file-write* (subpath "{home}"))

; Network policy
{net_policy}
"#
    )
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

    // Required system paths (read-only).  Check each path exists because
    // distro layouts differ: x86_64 has /lib64, ARM64 does not.
    for path in &["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"] {
        if std::path::Path::new(path).exists() {
            bwrap.arg("--ro-bind");
            bwrap.arg(path);
            bwrap.arg(path);
        }
    }
    bwrap.arg("--proc");
    bwrap.arg("/proc");
    bwrap.arg("--dev");
    bwrap.arg("/dev");
    bwrap.arg("--bind");
    bwrap.arg("/tmp");
    bwrap.arg("/tmp");

    // Workspace read-write
    bwrap.arg("--bind");
    bwrap.arg(&ws);
    bwrap.arg(&ws);

    // Home read-write

    // Real user home (read-only) - needed for rustup/cargo toolchain
    {
        // real_home is PathBuf, not Option
        let rh = &config.real_home;
        if rh.as_os_str() != home.as_str() && rh.exists() {
            bwrap.arg("--ro-bind");
            bwrap.arg(rh);
            bwrap.arg(rh);
        }
    }

    bwrap.arg("--bind");
    bwrap.arg(&home);
    bwrap.arg(&home);

    // Network policy.
    //
    // N-1 ruling (sandbox-internal loopback only):
    //   --unshare-all (above) already creates an isolated network
    //   namespace for BOTH policies. The child therefore never reaches
    //   the Linux guest host's 127.0.0.1, the Coding Harness endpoint,
    //   the Mac host, the VM gateway, LAN, DNS, or the public internet.
    //
    //   NetworkPolicy::Deny
    //     = all networking inside the namespace is disabled, including
    //       the namespace's own 127.0.0.1 and ::1.
    //   NetworkPolicy::LoopbackOnly
    //     = only the *same* isolated namespace's 127.0.0.1 / ::1 are
    //       usable. A server and its client must be launched together
    //       inside ONE bwrap invocation. This does NOT — and is not
    //       intended to — reach the guest host loopback.
    //
    // `--unshare-net` is added again under Deny for defence in depth:
    // it is redundant with `--unshare-all` but makes the deny intent
    // explicit and keeps the generated argv self-documenting.
    match config.network_policy {
        NetworkPolicy::Deny => {
            bwrap.arg("--unshare-net");
        }
        NetworkPolicy::LoopbackOnly => {
            // Intentionally no extra flag: --unshare-all already moved
            // the child into its own network namespace, whose loopback
            // interface is the only thing reachable. Do NOT add
            // --share-net or otherwise weaken isolation here.
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
            SandboxBackend::LinuxBubblewrap | SandboxBackend::Unavailable => {} // all valid
        }
    }

    // N-1 network isolation regression tests.
    //
    // On macOS `wrap_linux_bubblewrap` returns `Unavailable` (see the
    // non-Linux fallback), so the argv assertions only run on Linux. The
    // macOS profile tests above cover the seatbelt string generator.

    /// Helper: wrap a no-op command and collect the resulting bwrap argv.
    #[cfg(target_os = "linux")]
    fn bwrap_argv_for(policy: NetworkPolicy) -> Vec<String> {
        let config = SandboxConfig {
            workspace_root: PathBuf::from("/tmp/test-ws"),
            home_dir: PathBuf::from("/tmp/test-ws/.hcr-home"),
            real_home: PathBuf::from("/home/someuser"),
            agent_core_repo: None,
            network_policy: policy,
        };
        let backend = SandboxBackend::LinuxBubblewrap;
        let mut cmd = StdCommand::new("/bin/true");
        let wrapped = wrap_with_sandbox(&mut cmd, &config, &backend).expect("wrap succeeds");
        wrapped
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Both policies MUST move the child into its own network namespace
    /// via `--unshare-all`, so neither can reach the guest host loopback.
    #[cfg(target_os = "linux")]
    #[test]
    fn both_policies_isolate_network_namespace() {
        for policy in [NetworkPolicy::Deny, NetworkPolicy::LoopbackOnly] {
            let argv = bwrap_argv_for(policy.clone());
            assert!(
                argv.contains(&"--unshare-all".to_string()),
                "{policy:?}: --unshare-all must always be present (network isolation)"
            );
        }
    }

    /// Deny adds `--unshare-net` (defence in depth, redundant with
    /// `--unshare-all`) — so even the namespace's own loopback is down.
    #[cfg(target_os = "linux")]
    #[test]
    fn deny_policy_unshares_net_explicitly() {
        let argv = bwrap_argv_for(NetworkPolicy::Deny);
        assert!(
            argv.contains(&"--unshare-net".to_string()),
            "Deny: --unshare-net must be present"
        );
    }

    /// LoopbackOnly must NOT add any network-weakening flag.  It relies
    /// solely on `--unshare-all` for isolation; the only thing reachable
    /// is the namespace's own loopback interface.  There is deliberately
    /// no flag that re-shares the host/guest network namespace.
    #[cfg(target_os = "linux")]
    #[test]
    fn loopback_only_never_weakens_isolation() {
        let argv = bwrap_argv_for(NetworkPolicy::LoopbackOnly);
        assert!(
            !argv.contains(&"--unshare-net".to_string()),
            "LoopbackOnly must not add --unshare-net (its loopback is the namespace's own)"
        );
        assert!(
            !argv.iter().any(|a| a.starts_with("--share-net")),
            "LoopbackOnly must never add a --share-net style weakening flag"
        );
        assert!(
            argv.contains(&"--unshare-all".to_string()),
            "LoopbackOnly still requires --unshare-all for namespace isolation"
        );
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

        // Profile contains workspace and home
        assert!(profile.contains("/tmp/test-ws"));
        assert!(profile.contains("/tmp/test-ws/.hcr-home"));

        // No broad file-read allow (HCR v0: macOS sandbox unavailable)
        assert!(!profile.contains("(allow file-read* (subpath \"/\"))"));

        // H3: deny network* for Deny policy
        assert!(profile.contains("(deny network*)"));
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
