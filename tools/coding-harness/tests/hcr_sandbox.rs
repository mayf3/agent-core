//! HCR filesystem sandbox tests (14-19).
//!
//! Tests filesystem isolation properties of the HCR sandbox.
//! Sandbox tests are conditional on backend availability.

use std::collections::HashMap;
use std::path::PathBuf;

use coding_harness::hcr::executor::{self, HcrStatus};
use coding_harness::hcr::profile::{ArgTemplate, HcrCommandEntry, HcrProfile, NetworkPolicy};
use coding_harness::hcr::sandbox::SandboxBackend;

fn unique_test_dir(parent: &std::path::Path, name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!("{name}-{}-{nonce}", std::process::id()))
}

/// Helper: detect if sandbox is available for filesystem tests.
fn sandbox_available() -> bool {
    SandboxBackend::detect() != SandboxBackend::Unavailable
}

/// Build a profile with a simple cat/read command
fn sandbox_test_profile() -> HcrProfile {
    HcrProfile {
        id: "sandbox-test".into(),
        workspace_id: "test".into(),
        allowed_commands: vec![
            HcrCommandEntry {
                name: "read_file".into(),
                program: PathBuf::from("/bin/cat"),
                args: vec![ArgTemplate::Param("path".into())],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(5_000),
            },
            HcrCommandEntry {
                name: "write_file".into(),
                program: PathBuf::from("/usr/bin/touch"),
                args: vec![ArgTemplate::Param("path".into())],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(5_000),
            },
            HcrCommandEntry {
                name: "list_dir".into(),
                program: PathBuf::from("/bin/ls"),
                args: vec![ArgTemplate::Param("dir".into())],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(5_000),
            },
        ],
        ..Default::default()
    }
}

// ── Test 14: Child can read/write target workspace ──

#[test]
fn child_can_read_write_target_workspace() {
    let profile = sandbox_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_14");
    let _ = std::fs::create_dir_all(&ws);
    std::fs::write(ws.join("test.txt"), b"hello from workspace").unwrap();

    let mut params = HashMap::new();
    params.insert(
        "path".into(),
        ws.join("test.txt").to_string_lossy().to_string(),
    );

    let result = executor::execute(&profile, "read_file", &params, &ws);

    if sandbox_available() {
        assert_eq!(result.status, HcrStatus::Succeeded);
        assert!(
            result.stdout.contains("hello from workspace"),
            "child should read workspace file; stdout: {}",
            result.stdout
        );
    } else {
        assert_eq!(result.status, HcrStatus::Denied);
        assert_eq!(result.error_code, Some("HCR_SANDBOX_UNAVAILABLE".into()));
    }

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn workspace_under_home_remains_visible() {
    if !sandbox_available() {
        eprintln!("SKIP: sandbox not available for workspace-under-home test");
        return;
    }

    let real_home =
        PathBuf::from(std::env::var("HOME").expect("HOME must be configured for this test"));
    let ws = unique_test_dir(&real_home, ".hcr-workspace-under-home");
    std::fs::create_dir_all(&ws).unwrap();
    let input = ws.join("input.txt");
    std::fs::write(&input, b"workspace under home is visible").unwrap();

    let mut params = HashMap::new();
    params.insert("path".into(), input.to_string_lossy().to_string());
    let result = executor::execute(&sandbox_test_profile(), "read_file", &params, &ws);

    assert_eq!(result.status, HcrStatus::Succeeded, "{result:?}");
    assert_eq!(result.stdout, "workspace under home is visible");
    std::fs::remove_dir_all(&ws).unwrap();
}

#[test]
fn private_home_exists_and_does_not_shadow_workspace() {
    if !sandbox_available() {
        eprintln!("SKIP: sandbox not available for private-home test");
        return;
    }

    let real_home =
        PathBuf::from(std::env::var("HOME").expect("HOME must be configured for this test"));
    let ws = unique_test_dir(&real_home, ".hcr-private-home");
    std::fs::create_dir_all(&ws).unwrap();

    let mut profile = sandbox_test_profile();
    profile.allowed_commands.push(HcrCommandEntry {
        name: "print_home".into(),
        program: PathBuf::from("/usr/bin/printenv"),
        args: vec![ArgTemplate::Fixed("HOME".into())],
        network: Some(NetworkPolicy::Deny),
        timeout_ms_default: Some(5_000),
    });
    profile.allowed_commands.push(HcrCommandEntry {
        name: "list_home".into(),
        program: PathBuf::from("/bin/ls"),
        args: vec![ArgTemplate::Fixed("/run/agent-core-hcr/home".into())],
        network: Some(NetworkPolicy::Deny),
        timeout_ms_default: Some(5_000),
    });

    let home = executor::execute(&profile, "print_home", &HashMap::new(), &ws);
    let list = executor::execute(&profile, "list_home", &HashMap::new(), &ws);

    assert_eq!(home.status, HcrStatus::Succeeded, "{home:?}");
    assert_eq!(home.stdout.trim(), "/run/agent-core-hcr/home");
    assert!(!PathBuf::from(home.stdout.trim()).starts_with(&ws));
    assert_eq!(list.status, HcrStatus::Succeeded, "{list:?}");
    std::fs::remove_dir_all(&ws).unwrap();
}

// ── Test 15: Child cannot read agent-core repo ──

#[test]
fn child_cannot_read_agent_core_repo() {
    // Find the actual agent-core repo path
    let cwd = std::env::current_dir().unwrap_or_default();
    let agent_core_path = if cwd.join("Cargo.toml").exists() {
        cwd.clone()
    } else {
        // Skip if we can't find it
        return;
    };

    if !sandbox_available() {
        eprintln!("SKIP: sandbox not available for test 15");
        return;
    }

    let profile = sandbox_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_15");
    let _ = std::fs::create_dir_all(&ws);

    // Try to read a file from agent-core repo
    let target_file = agent_core_path.join("Cargo.toml");
    if !target_file.exists() {
        let _ = std::fs::remove_dir_all(&ws);
        eprintln!("SKIP: agent-core Cargo.toml not found");
        return;
    }

    let mut params = HashMap::new();
    params.insert("path".into(), target_file.to_string_lossy().to_string());

    let result = executor::execute(&profile, "read_file", &params, &ws);

    // With sandbox available, the repo should be denied.
    // Accept denied status or a failed execution (non-zero exit).
    assert!(
        result.status != HcrStatus::Succeeded,
        "agent-core repo should be blocked by sandbox; status={:?}",
        result.status,
    );

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 16: Child cannot read real home ──

#[test]
fn child_cannot_read_real_home() {
    if !sandbox_available() {
        eprintln!("SKIP: sandbox not available for test 16");
        return;
    }

    let profile = sandbox_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_16");
    let _ = std::fs::create_dir_all(&ws);

    let real_home = std::env::var("HOME").unwrap_or_else(|_| "/nonexistent".into());

    let mut params = HashMap::new();
    params.insert("path".into(), real_home);

    let result = executor::execute(&profile, "list_dir", &params, &ws);

    // With sandbox available, reading the real user home must be denied.
    assert!(
        result.status != HcrStatus::Succeeded,
        "reading real home should be blocked by sandbox; status={:?}",
        result.status,
    );

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 17: Child cannot read SSH key (denied path) ──

#[test]
fn child_cannot_read_fake_ssh_key() {
    if !sandbox_available() {
        eprintln!("SKIP: sandbox not available for test 17");
        return;
    }

    let profile = sandbox_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_17");
    let _ = std::fs::create_dir_all(&ws);

    // Create a canary file in the real home dir (which is denied by the
    // sandbox profile).  Keep it inside a temp subdirectory and clean up
    // afterwards so we don't pollute the user's home.
    let real_home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let canary_dir = std::path::Path::new(&real_home)
        .join("tmp")
        .join("hcr_test_17_canary");
    let _ = std::fs::create_dir_all(&canary_dir);
    let canary_file = canary_dir.join("id_rsa");
    std::fs::write(&canary_file, b"FAKE SSH KEY").unwrap();

    let mut params = HashMap::new();
    params.insert("path".into(), canary_file.to_string_lossy().to_string());

    let result = executor::execute(&profile, "read_file", &params, &ws);

    // The sandbox should deny access to files under real home.
    assert!(
        result.status != HcrStatus::Succeeded,
        "reading fake SSH key under real home should be blocked by sandbox; status={:?} stdout={:?}",
        result.status,
        result.stdout,
    );

    let _ = std::fs::remove_dir_all(&canary_dir);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn read_only_workspace_permissions_remain_enforced() {
    if !sandbox_available() {
        eprintln!("SKIP: sandbox not available for read-only workspace test");
        return;
    }

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let real_home =
        PathBuf::from(std::env::var("HOME").expect("HOME must be configured for this test"));
    let ws = unique_test_dir(&real_home, ".hcr-read-only-workspace");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o555)).unwrap();

    let output = ws.join("must-not-exist");
    let mut params = HashMap::new();
    params.insert("path".into(), output.to_string_lossy().to_string());
    let result = executor::execute(&sandbox_test_profile(), "write_file", &params, &ws);

    assert_ne!(result.status, HcrStatus::Succeeded, "{result:?}");
    assert!(!output.exists());
    std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_dir_all(&ws).unwrap();
}

// ── Test 18: Child cannot write outside workspace ──

#[test]
fn child_cannot_write_outside_workspace() {
    let profile = sandbox_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_18");
    let _ = std::fs::create_dir_all(&ws);

    let outside_path = std::env::temp_dir().join("hcr_test_18_outside.txt");
    // Ensure it doesn't exist yet
    let _ = std::fs::remove_file(&outside_path);

    // Use touch to try to create outside workspace
    let mut params = HashMap::new();
    params.insert("path".into(), outside_path.to_string_lossy().to_string());

    let _result = executor::execute(&profile, "write_file", &params, &ws);

    // Whether or not it succeeded, the outside file should NOT exist
    assert!(
        !outside_path.exists(),
        "child must not write outside workspace"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 19: Symlink escape remains rejected ──

#[test]
#[cfg(unix)]
fn symlink_escape_remains_rejected() {
    use std::os::unix::fs::symlink;

    let profile = sandbox_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_19");
    let _ = std::fs::create_dir_all(&ws);

    // Create a file outside workspace
    let outside_file = std::env::temp_dir().join("hcr_test_19_outside.txt");
    std::fs::write(&outside_file, b"sensitive data").unwrap();

    // Create a symlink inside workspace pointing outside
    let symlink_path = ws.join("escape.txt");
    symlink(&outside_file, &symlink_path).unwrap();

    let mut params = HashMap::new();
    params.insert("path".into(), symlink_path.to_string_lossy().to_string());

    let result = executor::execute(&profile, "read_file", &params, &ws);

    if sandbox_available() {
        assert_ne!(result.status, HcrStatus::Succeeded, "{result:?}");
        assert!(!result.stdout.contains("sensitive data"));
    }

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn path_traversal_is_rejected_before_spawn() {
    let mut params = HashMap::new();
    params.insert("path".into(), "../outside.txt".into());

    let result = executor::execute(
        &sandbox_test_profile(),
        "read_file",
        &params,
        &std::env::temp_dir(),
    );

    assert_eq!(result.status, HcrStatus::Denied);
    assert_eq!(result.error_code.as_deref(), Some("HCR_INVALID_PARAMETER"));
}
