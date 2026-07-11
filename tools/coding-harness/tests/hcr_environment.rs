//! HCR environment isolation tests (10-13).
//!
//! Tests that HCR child processes receive a properly isolated environment:
//! - env_clear() + allowlist
//! - HOME is not real user home
//! - No secret/env variable leakage
//!
//! When sandbox is unavailable, the executor fails closed with Denied.
//! These tests verify the environment contract regardless of sandbox state.

use std::collections::HashMap;
use std::path::PathBuf;

use coding_harness::hcr::executor::{self, HcrStatus};
use coding_harness::hcr::profile::{HcrCommandEntry, HcrProfile, NetworkPolicy};
use coding_harness::hcr::sandbox::SandboxBackend;

/// Build a minimal profile with an env-printing command.
fn env_profile() -> HcrProfile {
    HcrProfile {
        id: "test-env".into(),
        workspace_id: "test".into(),
        allowed_commands: vec![HcrCommandEntry {
            name: "print_env".into(),
            program: PathBuf::from("/usr/bin/env"),
            args: vec![],
            network: Some(NetworkPolicy::Deny),
            timeout_ms_default: Some(5_000),
        }],
        env_allowlist: vec!["PATH".into(), "HOME".into(), "TMPDIR".into()],
        ..Default::default()
    }
}

/// Returns true if sandbox is available for real process execution.
fn sandbox_unavailable() -> bool {
    SandboxBackend::detect() == SandboxBackend::Unavailable
}

/// Helper: run executor, handle both sandbox-available and unavailable cases.
/// When sandbox is unavailable, the executor returns Denied with
/// HCR_SANDBOX_UNAVAILABLE. We accept that as "environment contract valid."
fn execute_env_test(
    profile: &HcrProfile,
    command_name: &str,
    params: &HashMap<String, String>,
    ws: &PathBuf,
) -> executor::HcrExecResult {
    let result = executor::execute(profile, command_name, params, ws);
    if sandbox_unavailable() && result.status == HcrStatus::Denied {
        // Sandbox unavailable: executor correctly fails closed.
        // Verify it's the sandbox error, not a command policy error.
        assert_eq!(
            result.error_code,
            Some("HCR_SANDBOX_UNAVAILABLE".into()),
            "expected sandbox unavailable, got: {:?}",
            result.error_code
        );
    }
    result
}

// ── Test 10: Child does not see kernel fake token ──

#[test]
fn child_does_not_see_kernel_fake_token() {
    let profile = env_profile();
    let ws = std::env::temp_dir().join("hcr_test_10");
    let _ = std::fs::create_dir_all(&ws);

    // Set a fake API key in the parent process env
    if std::env::var("OPENAI_API_KEY").is_err() {
        unsafe { std::env::set_var("OPENAI_API_KEY", "sk-fake-secret-12345") };
    }
    if std::env::var("DEEPSEEK_API_KEY").is_err() {
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "ds-fake-secret") };
    }

    let params = HashMap::new();
    let result = execute_env_test(&profile, "print_env", &params, &ws);

    // Clean up the env vars we set
    unsafe { std::env::remove_var("OPENAI_API_KEY") };
    unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };

    if !sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Succeeded);
        assert!(
            !result.stdout.contains("OPENAI_API_KEY"),
            "child must not see OPENAI_API_KEY"
        );
        assert!(
            !result.stdout.contains("DEEPSEEK_API_KEY"),
            "child must not see DEEPSEEK_API_KEY"
        );
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 11: Child does not see SSH_AUTH_SOCK ──

#[test]
fn child_does_not_see_ssh_auth_sock() {
    let profile = env_profile();
    let ws = std::env::temp_dir().join("hcr_test_11");
    let _ = std::fs::create_dir_all(&ws);

    // Ensure the parent has SSH_AUTH_SOCK set
    let had_ssh = std::env::var("SSH_AUTH_SOCK").ok();
    if had_ssh.is_none() {
        unsafe { std::env::set_var("SSH_AUTH_SOCK", "/tmp/ssh-agent.sock") };
    }

    let params = HashMap::new();
    let result = execute_env_test(&profile, "print_env", &params, &ws);

    if had_ssh.is_none() {
        unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
    }

    if !sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Succeeded);
        assert!(
            !result.stdout.contains("SSH_AUTH_SOCK"),
            "child must not see SSH_AUTH_SOCK"
        );
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 12: Child home is not real user home ──

#[test]
fn child_home_is_not_real_user_home() {
    let profile = env_profile();
    let ws = std::env::temp_dir().join("hcr_test_12");
    let _ = std::fs::create_dir_all(&ws);

    let real_home = std::env::var("HOME").unwrap_or_else(|_| "/nonexistent".into());

    let params = HashMap::new();
    let result = execute_env_test(&profile, "print_env", &params, &ws);

    if !sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Succeeded);
        // The child HOME should NOT be the real user home.  Check for
        // the "HOME=<real_home>" line specifically, rather than a plain
        // substring match, because PATH may legitimately contain the
        // home directory (e.g. /Users/yanfenma/bin).
        let home_line = format!("HOME={}", real_home);
        assert!(
            !result.stdout.contains(&home_line),
            "child HOME must not be real user home: {}",
            real_home
        );
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 13: Child receives only allowlisted env ──

#[test]
fn child_receives_only_allowlisted_env() {
    let profile = env_profile();
    let ws = std::env::temp_dir().join("hcr_test_13");
    let _ = std::fs::create_dir_all(&ws);

    // Set a TEST_SECRET env var that is not in allowlist
    if std::env::var("TEST_SECRET").is_err() {
        unsafe { std::env::set_var("TEST_SECRET", "should-not-leak") };
    }
    if std::env::var("MY_CUSTOM_VAR").is_err() {
        unsafe { std::env::set_var("MY_CUSTOM_VAR", "also-not-leaked") };
    }

    let params = HashMap::new();
    let result = execute_env_test(&profile, "print_env", &params, &ws);

    unsafe {
        std::env::remove_var("TEST_SECRET");
        std::env::remove_var("MY_CUSTOM_VAR");
    }

    if !sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Succeeded);

        // Count allowlisted vars in output
        let has_path = result.stdout.contains("PATH=");
        let has_home = result.stdout.contains("HOME=");
        let has_tmpdir = result.stdout.contains("TMPDIR=");

        // PATH, HOME, TMPDIR should be present
        assert!(has_path, "PATH must be in child env");
        assert!(has_home, "HOME must be in child env");
        assert!(has_tmpdir, "TMPDIR must be in child env");

        // Non-allowlisted vars must NOT be present
        assert!(
            !result.stdout.contains("TEST_SECRET"),
            "TEST_SECRET must NOT be in child env"
        );
        assert!(
            !result.stdout.contains("MY_CUSTOM_VAR"),
            "MY_CUSTOM_VAR must NOT be in child env"
        );
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test: Denied command returns proper error ──

#[test]
fn denied_command_returns_structured_error() {
    let profile = env_profile();
    let ws = std::env::temp_dir().join("hcr_test_denied");
    let _ = std::fs::create_dir_all(&ws);

    let params = HashMap::new();
    let result = executor::execute(&profile, "nonexistent_cmd", &params, &ws);

    // Command policy check happens BEFORE sandbox, so this always works
    assert_eq!(result.status, HcrStatus::Denied);
    assert_eq!(result.error_code, Some("HCR_COMMAND_NOT_ALLOWED".into()));
    assert!(!result.stderr.is_empty());

    let _ = std::fs::remove_dir_all(&ws);
}
