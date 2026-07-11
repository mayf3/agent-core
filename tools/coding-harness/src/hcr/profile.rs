//! HCR execution profile configuration model.
//!
//! Defines the HCR profile structure that controls what commands, network
//! access, and environment are allowed during HCR execution. Profiles are
//! configured via `CODING_CONFIG` and explicitly selected at request time.

use std::path::PathBuf;

/// Per-command network access policy.
///
/// Both variants execute inside an **isolated network namespace** created
/// unconditionally by `--unshare-all` in the bubblewrap wrapper.  The
/// distinction is therefore about what is reachable *inside that
/// namespace*, not about reaching the Linux guest host or the Mac host.
///
/// See `tools/coding-harness/src/hcr/sandbox.rs` for the full N-1 ruling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// No network access allowed — not even the namespace's own loopback.
    Deny,
    /// Sandbox-internal loopback only: the namespace's own `127.0.0.1` /
    /// `::1` are usable, so a server and its client must run together in
    /// one bubblewrap invocation.  This does **not** reach the Linux guest
    /// host's loopback, the Coding Harness endpoint, the Mac host, the VM
    /// gateway, LAN, DNS, or the public internet.
    LoopbackOnly,
}

/// A template argument for a command entry.
///
/// Templates describe how to construct the command line from caller-supplied
/// parameters and fixed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgTemplate {
    /// A fixed argument that is always the same.
    Fixed(String),
    /// A caller-supplied parameter, validated by the command entry.
    Param(String),
}

/// A single allowed command in an HCR profile.
///
/// Each entry defines a named, fixed-executable command with structured
/// argument templates. The executable path is fixed at configuration time;
/// the caller provides only the parameter values.
#[derive(Debug, Clone)]
pub struct HcrCommandEntry {
    /// Logical command name used in requests (e.g. "node_test").
    pub name: String,
    /// Absolute path to the trusted executable or script.
    pub program: PathBuf,
    /// Structured argument templates (positional).
    pub args: Vec<ArgTemplate>,
    /// Per-command network policy (overrides profile default if set).
    pub network: Option<NetworkPolicy>,
    /// Default timeout in milliseconds for this command.
    pub timeout_ms_default: Option<u64>,
}

/// An HCR execution profile.
///
/// Profiles are configured in `CODING_CONFIG` and selected by `profile_id`
/// at request time. A profile is only activated when a valid `hcr_token`
/// is provided.
#[derive(Debug, Clone)]
pub struct HcrProfile {
    /// Profile identifier (e.g. "hcr-v0").
    pub id: String,
    /// Workspace this profile binds to.
    pub workspace_id: String,
    /// Allowed commands with their templates.
    pub allowed_commands: Vec<HcrCommandEntry>,
    /// Environment variable allowlist (names only).
    pub env_allowlist: Vec<String>,
    /// Default network policy for this profile.
    pub network_policy: NetworkPolicy,
    /// Maximum timeout in milliseconds for any command.
    pub timeout_ms_max: u64,
    /// Maximum output bytes for stdout or stderr.
    pub output_bytes_max: usize,
    /// Optional sandbox home directory.
    ///
    /// If set, `HOME` in the child will point here instead of the real user
    /// home. If unset, a temporary directory is created per execution.
    pub sandbox_home: Option<PathBuf>,
}

impl Default for HcrProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            workspace_id: String::new(),
            allowed_commands: Vec::new(),
            env_allowlist: vec![
                "PATH".into(),
                "TMPDIR".into(),
                "HOME".into(),
                "LANG".into(),
                "LC_ALL".into(),
                "LC_CTYPE".into(),
            ],
            network_policy: NetworkPolicy::Deny,
            timeout_ms_max: 120_000,
            output_bytes_max: 1_048_576,
            sandbox_home: None,
        }
    }
}

impl HcrProfile {
    /// Look up a command entry by name.
    pub fn find_command(&self, name: &str) -> Option<&HcrCommandEntry> {
        self.allowed_commands.iter().find(|c| c.name == name)
    }

    /// Return the effective network policy for a command.
    pub fn effective_network(&self, cmd: &HcrCommandEntry) -> NetworkPolicy {
        cmd.network
            .clone()
            .unwrap_or_else(|| self.network_policy.clone())
    }
}

/// Build the default "hcr-v0" profile with the three standard commands.
///
/// The paths are configured at runtime from the profile config; this function
/// builds an example for documentation and testing.
pub fn default_hcr_v0_profile(workspace_id: &str, harness_root: &PathBuf) -> HcrProfile {
    let scaffold_script = harness_root.join("..").join("scaffold-context-harness.sh");
    let smoke_runner = harness_root.join("..").join("smoke-context-harness.mjs");

    HcrProfile {
        id: "hcr-v0".into(),
        workspace_id: workspace_id.into(),
        allowed_commands: vec![
            HcrCommandEntry {
                name: "scaffold_context_harness".into(),
                program: scaffold_script,
                args: vec![
                    ArgTemplate::Param("harness_id".into()),
                    ArgTemplate::Fixed("--root".into()),
                    ArgTemplate::Param("harness_root".into()),
                ],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(30_000),
            },
            HcrCommandEntry {
                name: "node_test".into(),
                program: PathBuf::from("/usr/bin/env"),
                args: vec![
                    ArgTemplate::Fixed("node".into()),
                    ArgTemplate::Fixed("--test".into()),
                    ArgTemplate::Param("test_path".into()),
                ],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(60_000),
            },
            HcrCommandEntry {
                name: "harness_local_smoke".into(),
                program: PathBuf::from("/usr/bin/env"),
                args: vec![
                    ArgTemplate::Fixed("node".into()),
                    ArgTemplate::Fixed(smoke_runner.to_string_lossy().into_owned()),
                    ArgTemplate::Fixed("--manifest".into()),
                    ArgTemplate::Param("manifest_path".into()),
                ],
                network: Some(NetworkPolicy::LoopbackOnly),
                timeout_ms_default: Some(120_000),
            },
        ],
        ..Default::default()
    }
}

/// Parse an HcrProfile from a JSON value (sub-object of `hcr_profiles`).
pub fn parse_profile_from_json(value: &serde_json::Value) -> Option<HcrProfile> {
    let id = value.get("id")?.as_str()?;
    let workspace_id = value.get("workspace_id")?.as_str()?;

    let mut profile = HcrProfile {
        id: id.to_string(),
        workspace_id: workspace_id.to_string(),
        ..Default::default()
    };

    // Parse allowed_commands
    if let Some(cmds) = value.get("allowed_commands").and_then(|v| v.as_array()) {
        for cmd_val in cmds {
            let name = cmd_val.get("name")?.as_str()?;
            let program_str = cmd_val.get("program")?.as_str()?;
            let mut entry = HcrCommandEntry {
                name: name.to_string(),
                program: PathBuf::from(program_str),
                args: Vec::new(),
                network: None,
                timeout_ms_default: None,
            };
            if let Some(args_arr) = cmd_val.get("args").and_then(|v| v.as_array()) {
                for arg_val in args_arr {
                    if let Some(fixed) = arg_val.get("Fixed").and_then(|v| v.as_str()) {
                        entry.args.push(ArgTemplate::Fixed(fixed.to_string()));
                    } else if let Some(param) = arg_val.get("Param").and_then(|v| v.as_str()) {
                        entry.args.push(ArgTemplate::Param(param.to_string()));
                    }
                }
            }
            if let Some(net) = cmd_val.get("network").and_then(|v| v.as_str()) {
                entry.network = Some(match net {
                    "loopback_only" => NetworkPolicy::LoopbackOnly,
                    _ => NetworkPolicy::Deny,
                });
            }
            entry.timeout_ms_default = cmd_val.get("timeout_ms").and_then(|v| v.as_u64());
            profile.allowed_commands.push(entry);
        }
    }

    // Parse env_allowlist
    if let Some(env_list) = value.get("env_allowlist").and_then(|v| v.as_array()) {
        profile.env_allowlist = env_list
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    // Parse network_policy
    if let Some(net) = value.get("network_policy").and_then(|v| v.as_str()) {
        profile.network_policy = match net {
            "loopback_only" => NetworkPolicy::LoopbackOnly,
            _ => NetworkPolicy::Deny,
        };
    }

    // Parse numeric limits
    if let Some(t) = value.get("timeout_ms_max").and_then(|v| v.as_u64()) {
        profile.timeout_ms_max = t;
    }
    if let Some(o) = value.get("output_bytes_max").and_then(|v| v.as_u64()) {
        profile.output_bytes_max = o as usize;
    }

    // Parse sandbox_home
    if let Some(h) = value.get("sandbox_home").and_then(|v| v.as_str()) {
        profile.sandbox_home = Some(PathBuf::from(h));
    }

    Some(profile)
}

/// Serialize an HcrProfile to a JSON value (for testing/debugging).
pub fn profile_to_json(profile: &HcrProfile) -> serde_json::Value {
    let cmds: Vec<serde_json::Value> = profile
        .allowed_commands
        .iter()
        .map(|cmd| {
            let args: Vec<serde_json::Value> = cmd
                .args
                .iter()
                .map(|a| match a {
                    ArgTemplate::Fixed(s) => serde_json::json!({"Fixed": s}),
                    ArgTemplate::Param(s) => serde_json::json!({"Param": s}),
                })
                .collect();
            let mut cmd_json = serde_json::json!({
                "name": cmd.name,
                "program": cmd.program.to_string_lossy(),
                "args": args,
            });
            if let Some(ref net) = cmd.network {
                let ns = match net {
                    NetworkPolicy::Deny => "deny",
                    NetworkPolicy::LoopbackOnly => "loopback_only",
                };
                cmd_json["network"] = serde_json::json!(ns);
            }
            if let Some(t) = cmd.timeout_ms_default {
                cmd_json["timeout_ms"] = serde_json::json!(t);
            }
            cmd_json
        })
        .collect();

    let net_str = match profile.network_policy {
        NetworkPolicy::Deny => "deny",
        NetworkPolicy::LoopbackOnly => "loopback_only",
    };

    let mut result = serde_json::json!({
        "id": profile.id,
        "workspace_id": profile.workspace_id,
        "allowed_commands": cmds,
        "env_allowlist": profile.env_allowlist,
        "network_policy": net_str,
        "timeout_ms_max": profile.timeout_ms_max,
        "output_bytes_max": profile.output_bytes_max,
    });
    if let Some(ref home) = profile.sandbox_home {
        result["sandbox_home"] = serde_json::json!(home.to_string_lossy());
    }
    result
}
