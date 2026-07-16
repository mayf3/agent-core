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
    // Give the child a private temporary filesystem. Binding the host /tmp
    // read-write would let candidate code mutate files outside its workspace.
    // A workspace located below /tmp is bound back explicitly immediately
    // afterwards, so build output remains available to the Harness.
    bwrap.arg("--tmpfs");
    bwrap.arg("/tmp");

    // Workspace read-write
    bwrap.arg("--bind");
    bwrap.arg(&ws);
    bwrap.arg(&ws);

    // Private sandbox home (tmpfs — NOT host /home).
    // The candidate sees an empty, writable /home that disappears
    // when the sandbox exits.  Host home, SSH keys, gitconfig, and
    // config directories are NOT accessible.
    bwrap.arg("--tmpfs");
    bwrap.arg("/home");
    bwrap.arg("--dir");
    bwrap.arg("/home/sandbox");

    // Rust toolchain: read-only bind of specific host paths to
    // custom sandbox locations.  Resolve RUSTUP_HOME and CARGO_HOME
    // directly from the host environment (the callers set them before
    // invocation).  Only the directories actually needed for cargo/rustc
    // are exposed — NOT the entire host /home.
    let mut sandbox_rustup = None;
    let mut sandbox_cargo = None;

    let host_rustup = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{h}/.rustup"))
            .unwrap_or_default()
    });
    if !host_rustup.is_empty() && std::path::Path::new(&host_rustup).exists() {
        let sb = "/opt/rustup";
        bwrap.arg("--ro-bind");
        bwrap.arg(&host_rustup);
        bwrap.arg(sb);
        sandbox_rustup = Some(sb.to_string());
    }

    let host_cargo = std::env::var("CARGO_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{h}/.cargo"))
            .unwrap_or_default()
    });
    // Create /opt directory structure for sandbox cargo/rustup paths
    bwrap.arg("--dir");
    bwrap.arg("/opt");
    bwrap.arg("--dir");
    bwrap.arg("/opt/cargo");

    // Read-only bind of ONLY the cargo subdirectories required for building.
    // The host's entire ~/.cargo is intentionally NOT mounted — that would
    // expose credentials.toml, credentials, config.toml, config, and any
    // private registry tokens.
    //
    // Only bind: bin, registry, git, .crates metadata files.
    if !host_cargo.is_empty() && std::path::Path::new(&host_cargo).exists() {
        let sb = "/opt/cargo";
        sandbox_cargo = Some(sb.to_string());
        for sub in &["bin", "registry", "git", ".crates.toml", ".crates2.json"] {
            let host_sub = format!("{host_cargo}/{sub}");
            let sb_sub = format!("{sb}/{sub}");
            if std::path::Path::new(&host_sub).exists() {
                bwrap.arg("--ro-bind");
                bwrap.arg(&host_sub);
                bwrap.arg(&sb_sub);
            }
        }
    }

    // Sandbox home (writable, per-gate)
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

    // Override HOME, RUSTUP_HOME, CARGO_HOME to sandbox paths.
    // HOME → private /home/sandbox (tmpfs, not host home)
    // RUSTUP_HOME → /opt/rustup (ro-bind from host)
    // CARGO_HOME → /opt/cargo (ro-bind from host)
    bwrap.env("HOME", "/home/sandbox");
    if let Some(rustup) = &sandbox_rustup {
        bwrap.env("RUSTUP_HOME", rustup);
    }
    if let Some(cargo) = &sandbox_cargo {
        bwrap.env("CARGO_HOME", cargo);
        // Prepend sandbox cargo bin dir to PATH so `cargo` is found
        let cargo_bin = format!("{cargo}/bin");
        let path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{cargo_bin}:{path}");
        bwrap.env("PATH", &new_path);
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
#[path = "sandbox_tests.rs"]
mod tests;
