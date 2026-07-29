use super::*;
use std::path::PathBuf;

#[test]
fn detect_backend_never_panics() {
    let backend = SandboxBackend::detect();
    match backend {
        SandboxBackend::LinuxBubblewrap | SandboxBackend::Unavailable => {}
    }
}

#[cfg(target_os = "linux")]
fn bwrap_command_for(policy: NetworkPolicy, workspace: &str) -> StdCommand {
    let config = SandboxConfig {
        workspace_root: PathBuf::from(workspace),
        home_dir: PathBuf::from(format!("{workspace}/.hcr-home")),
        real_home: PathBuf::from("/home/someuser"),
        agent_core_repo: None,
        network_policy: policy,
    };
    let backend = SandboxBackend::LinuxBubblewrap;
    let mut command = StdCommand::new("/bin/true");
    wrap_with_sandbox(&mut command, &config, &backend).expect("wrap succeeds")
}

#[cfg(target_os = "linux")]
fn bwrap_argv_for(policy: NetworkPolicy) -> Vec<String> {
    bwrap_command_for(policy, "/tmp/test-ws")
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

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

#[cfg(target_os = "linux")]
#[test]
fn deny_policy_unshares_net_explicitly() {
    let argv = bwrap_argv_for(NetworkPolicy::Deny);
    assert!(
        argv.contains(&"--unshare-net".to_string()),
        "Deny: --unshare-net must be present"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn temporary_directory_is_private() {
    let argv = bwrap_argv_for(NetworkPolicy::Deny);
    assert!(argv.windows(2).any(|args| args == ["--tmpfs", "/tmp"]));
    assert!(!argv
        .windows(3)
        .any(|args| args == ["--bind", "/tmp", "/tmp"]));
}

#[cfg(target_os = "linux")]
#[test]
fn workspace_under_home_is_not_shadowed() {
    let workspace = "/home/someuser/workspace";
    let argv: Vec<_> = bwrap_command_for(NetworkPolicy::Deny, workspace)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert!(argv
        .windows(3)
        .any(|args| { args == ["--bind", workspace, workspace] }));
    assert!(argv.windows(2).any(|args| args == ["--dir", "/home"]));
    assert!(argv
        .windows(2)
        .any(|args| args == ["--dir", "/home/someuser"]));
    assert!(!argv.windows(2).any(|args| args == ["--tmpfs", "/home"]));
}

#[cfg(target_os = "linux")]
#[test]
fn private_home_is_independent_tmpfs_and_home_env_points_to_it() {
    let command = bwrap_command_for(NetworkPolicy::Deny, "/home/someuser/workspace");
    let argv: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let home = command
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new("HOME"))
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned());

    assert!(argv
        .windows(2)
        .any(|args| args == ["--tmpfs", LINUX_PRIVATE_HOME]));
    assert!(!argv
        .windows(3)
        .any(|args| args[0] == "--bind" && args[1].ends_with(".hcr-home")));
    assert_eq!(home.as_deref(), Some(LINUX_PRIVATE_HOME));
}

#[cfg(target_os = "linux")]
#[test]
fn workspace_and_private_home_overlap_fails_closed() {
    let config = SandboxConfig {
        workspace_root: PathBuf::from("/run/agent-core-hcr"),
        home_dir: PathBuf::from("/tmp/unused-home"),
        real_home: PathBuf::from("/home/someuser"),
        agent_core_repo: None,
        network_policy: NetworkPolicy::Deny,
    };
    let backend = SandboxBackend::LinuxBubblewrap;
    let mut command = StdCommand::new("/bin/true");
    let error = wrap_with_sandbox(&mut command, &config, &backend).unwrap_err();

    assert_eq!(error.error_code(), "HCR_INTERNAL_ERROR");
    assert!(error.to_string().contains("overlaps private HOME"));
}

#[cfg(target_os = "linux")]
#[test]
fn loopback_only_never_weakens_isolation() {
    let argv = bwrap_argv_for(NetworkPolicy::LoopbackOnly);
    assert!(
        !argv.contains(&"--unshare-net".to_string()),
        "LoopbackOnly must not add --unshare-net (its loopback is the namespace's own)"
    );
    assert!(
        !argv.iter().any(|arg| arg.starts_with("--share-net")),
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
    assert!(profile.contains("/tmp/test-ws"));
    assert!(profile.contains("/tmp/test-ws/.hcr-home"));
    assert!(!profile.contains("(allow file-read* (subpath \"/\"))"));
    assert!(profile.contains("(deny network*)"));
    assert!(!profile.contains("remote ip \"localhost:*\""));
    assert!(profile.contains("(allow process-exec)"));
    assert!(profile.contains("(allow process-fork)"));
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
    assert!(profile.contains("(deny network*)"));
    assert!(profile.contains("(allow network* (remote ip \"localhost:*\"))"));
    assert!(!profile.contains("(local ip"));
    assert!(!profile.contains("\"127.0.0.1\""));
    assert!(!profile.contains("\"::1\""));
}

#[test]
fn unavailable_backend_fails_closed() {
    let backend = SandboxBackend::Unavailable;
    let mut command = StdCommand::new("echo");
    let config = SandboxConfig {
        workspace_root: PathBuf::from("/tmp/ws"),
        home_dir: PathBuf::from("/tmp/ws/home"),
        real_home: PathBuf::from("/Users/user"),
        agent_core_repo: None,
        network_policy: NetworkPolicy::Deny,
    };
    let result = wrap_with_sandbox(&mut command, &config, &backend);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().error_code(), "HCR_SANDBOX_UNAVAILABLE");
}

#[test]
fn describe_sandbox_never_empty() {
    assert!(!describe_sandbox_status().is_empty());
}
