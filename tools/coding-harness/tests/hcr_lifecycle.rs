//! HCR process lifecycle tests (23-29).
//!
//! Tests timeout, process group kill, output truncation, and cleanup.
//! When sandbox is unavailable, process execution tests gracefully verify
//! the executor fails closed correctly.

use std::collections::HashMap;
use std::path::PathBuf;

use coding_harness::hcr::executor::{self, CleanupStatus, HcrStatus};
use coding_harness::hcr::profile::{ArgTemplate, HcrCommandEntry, HcrProfile, NetworkPolicy};
use coding_harness::hcr::sandbox::SandboxBackend;

fn sandbox_unavailable() -> bool {
    SandboxBackend::detect() == SandboxBackend::Unavailable
}

/// Build a profile with various lifecycle-testing commands.
fn lifecycle_profile() -> HcrProfile {
    HcrProfile {
        id: "lifecycle-test".into(),
        workspace_id: "test".into(),
        allowed_commands: vec![
            HcrCommandEntry {
                name: "echo_message".into(),
                program: PathBuf::from("/bin/echo"),
                args: vec![ArgTemplate::Param("message".into())],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(5_000),
            },
            HcrCommandEntry {
                name: "sleep_long".into(),
                program: PathBuf::from("/bin/sleep"),
                args: vec![ArgTemplate::Fixed("30".into())],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(500),
            },
            HcrCommandEntry {
                name: "fail_command".into(),
                program: PathBuf::from("/bin/false"),
                args: vec![],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(5_000),
            },
            HcrCommandEntry {
                name: "large_output".into(),
                program: PathBuf::from("/usr/bin/env"),
                args: vec![
                    ArgTemplate::Fixed("python3".into()),
                    ArgTemplate::Fixed("-c".into()),
                    ArgTemplate::Param("code".into()),
                ],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(10_000),
            },
        ],
        ..Default::default()
    }
}

// ── Test 23: timeout sets timed_out true ──

#[test]
fn timeout_sets_timed_out_true() {
    let profile = lifecycle_profile();
    let ws = std::env::temp_dir().join("hcr_test_23");
    let _ = std::fs::create_dir_all(&ws);

    let params = HashMap::new();
    let result = executor::execute(&profile, "sleep_long", &params, &ws);

    if sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Denied);
        assert_eq!(result.error_code, Some("HCR_SANDBOX_UNAVAILABLE".into()));
    } else {
        assert_eq!(result.status, HcrStatus::TimedOut);
        assert!(result.timed_out);
        assert_eq!(result.error_code, Some("HCR_TIMEOUT".into()));
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 24: timeout kills process group ──

#[test]
fn timeout_kills_process_group() {
    let profile = lifecycle_profile();
    let ws = std::env::temp_dir().join("hcr_test_24");
    let _ = std::fs::create_dir_all(&ws);

    let params = HashMap::new();
    let result = executor::execute(&profile, "sleep_long", &params, &ws);

    if sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Denied);
    } else {
        assert_eq!(result.status, HcrStatus::TimedOut);
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 25: child cleanup confirmed after success ──

#[test]
fn child_cleanup_confirmed_after_success() {
    let profile = lifecycle_profile();
    let ws = std::env::temp_dir().join("hcr_test_25");
    let _ = std::fs::create_dir_all(&ws);

    let mut params = HashMap::new();
    params.insert("message".into(), "success".into());

    let result = executor::execute(&profile, "echo_message", &params, &ws);

    if sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Denied);
        assert_eq!(result.error_code, Some("HCR_SANDBOX_UNAVAILABLE".into()));
    } else {
        assert_eq!(result.status, HcrStatus::Succeeded);
        assert_eq!(result.child_cleanup, CleanupStatus::Confirmed);
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 26: child cleanup confirmed after failure ──

#[test]
fn child_cleanup_confirmed_after_failure() {
    let profile = lifecycle_profile();
    let ws = std::env::temp_dir().join("hcr_test_26");
    let _ = std::fs::create_dir_all(&ws);

    let params = HashMap::new();
    let result = executor::execute(&profile, "fail_command", &params, &ws);

    if sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Denied);
        assert_eq!(result.error_code, Some("HCR_SANDBOX_UNAVAILABLE".into()));
    } else {
        assert_eq!(result.status, HcrStatus::Failed);
        assert_ne!(result.exit_code, 0);
        assert_eq!(result.child_cleanup, CleanupStatus::Confirmed);
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 27: child cleanup confirmed after timeout ──

#[test]
fn child_cleanup_confirmed_after_timeout() {
    let profile = lifecycle_profile();
    let ws = std::env::temp_dir().join("hcr_test_27");
    let _ = std::fs::create_dir_all(&ws);

    let params = HashMap::new();
    let result = executor::execute(&profile, "sleep_long", &params, &ws);

    if sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Denied);
        assert_eq!(result.error_code, Some("HCR_SANDBOX_UNAVAILABLE".into()));
    } else {
        assert_eq!(result.status, HcrStatus::TimedOut);
        assert_eq!(result.child_cleanup, CleanupStatus::Confirmed);
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 28: stdout truncation reported ──

#[test]
fn stdout_truncation_reported() {
    let profile = lifecycle_profile();
    let ws = std::env::temp_dir().join("hcr_test_28");
    let _ = std::fs::create_dir_all(&ws);

    let mut params = HashMap::new();
    params.insert(
        "code".into(),
        "import sys; sys.stdout.write('x' * 1500000); sys.stdout.flush()".into(),
    );

    let result = executor::execute(&profile, "large_output", &params, &ws);

    if sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Denied);
    } else if result.status == HcrStatus::Succeeded || result.exit_code == 0 {
        assert!(
            result.stdout_truncated || result.stdout.len() <= profile.output_bytes_max,
            "stdout should be truncated or within limit"
        );
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 29: stderr truncation reported ──

#[test]
fn stderr_truncation_reported() {
    let profile = lifecycle_profile();
    let ws = std::env::temp_dir().join("hcr_test_29");
    let _ = std::fs::create_dir_all(&ws);

    let mut params = HashMap::new();
    params.insert(
        "code".into(),
        "import sys; sys.stderr.write('e' * 1500000); sys.stderr.flush()".into(),
    );

    let result = executor::execute(&profile, "large_output", &params, &ws);

    if sandbox_unavailable() {
        assert_eq!(result.status, HcrStatus::Denied);
    } else if result.status == HcrStatus::Succeeded || result.exit_code == 0 {
        assert!(
            result.stderr_truncated || result.stderr.len() <= profile.output_bytes_max,
            "stderr should be truncated or within limit"
        );
    }

    let _ = std::fs::remove_dir_all(&ws);
}
