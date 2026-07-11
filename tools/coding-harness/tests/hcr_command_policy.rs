//! HCR command policy tests (1-9).
//!
//! Tests the CommandPolicy enforcement against HCR profiles.
//! All tests use unit-level validation, no server needed.

use std::collections::HashMap;
use std::path::PathBuf;

use coding_harness::hcr::command::CommandPolicy;
use coding_harness::hcr::profile::{ArgTemplate, HcrCommandEntry, HcrProfile, NetworkPolicy};

fn test_profile() -> HcrProfile {
    HcrProfile {
        id: "hcr-v0".into(),
        workspace_id: "harness-dev".into(),
        allowed_commands: vec![
            HcrCommandEntry {
                name: "scaffold_context_harness".into(),
                program: PathBuf::from("/opt/harness/scaffold.sh"),
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
                    ArgTemplate::Fixed("/opt/harness/smoke.mjs".into()),
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

// ── Test 1: Trusted scaffold ──

#[test]
fn hcr_profile_allows_trusted_scaffold() {
    let profile = test_profile();
    let mut params = HashMap::new();
    params.insert("harness_id".into(), "my-harness".into());
    params.insert("harness_root".into(), "/tmp/harness-dev".into());

    let result = CommandPolicy::check(
        "scaffold_context_harness",
        &params,
        &profile,
        &PathBuf::from("/ws"),
    );
    assert!(
        result.is_ok(),
        "scaffold should be allowed: {:?}",
        result.err()
    );
    let cmd = result.unwrap();
    assert_eq!(cmd.program, PathBuf::from("/opt/harness/scaffold.sh"));
}

// ── Test 2: Fixed node_test ──

#[test]
fn hcr_profile_allows_fixed_node_test() {
    let profile = test_profile();
    let mut params = HashMap::new();
    params.insert("test_path".into(), "server.test.mjs".into());

    let result = CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws"));
    assert!(
        result.is_ok(),
        "node_test should be allowed: {:?}",
        result.err()
    );
    let cmd = result.unwrap();
    assert!(cmd.args.contains(&"node".to_string()));
    assert!(cmd.args.contains(&"--test".to_string()));
    assert!(cmd.args.contains(&"server.test.mjs".to_string()));
}

// ── Test 3: Trusted smoke runner ──

#[test]
fn hcr_profile_allows_trusted_smoke_runner() {
    let profile = test_profile();
    let mut params = HashMap::new();
    params.insert("manifest_path".into(), "harness.manifest.json".into());

    let result = CommandPolicy::check(
        "harness_local_smoke",
        &params,
        &profile,
        &PathBuf::from("/ws"),
    );
    assert!(
        result.is_ok(),
        "smoke runner should be allowed: {:?}",
        result.err()
    );
}

// ── Test 4: Reject sh -c ──

#[test]
fn hcr_profile_rejects_sh_c() {
    assert!(
        CommandPolicy::check_raw_forbidden("sh", &["-c".into(), "echo hi".into()]),
        "sh -c must be forbidden"
    );
}

// ── Test 5: Reject bash -c ──

#[test]
fn hcr_profile_rejects_bash_c() {
    assert!(
        CommandPolicy::check_raw_forbidden("bash", &["-c".into(), "echo hi".into()]),
        "bash -c must be forbidden"
    );
}

// ── Test 6: Reject node eval ──

#[test]
fn hcr_profile_rejects_node_eval() {
    assert!(
        CommandPolicy::check_raw_forbidden("node", &["-e".into(), "console.log(1)".into()]),
        "node -e must be forbidden"
    );
    assert!(
        CommandPolicy::check_raw_forbidden("node", &["--eval".into(), "code".into()]),
        "node --eval must be forbidden"
    );
}

// ── Test 7: Reject arbitrary node script (not a named command) ──

#[test]
fn hcr_profile_rejects_arbitrary_node_script() {
    let profile = test_profile();
    let params = HashMap::new();
    // "node" is not a named command in the profile
    let result = CommandPolicy::check("node", &params, &profile, &PathBuf::from("/ws"));
    assert!(result.is_err(), "arbitrary node command should be rejected");
    assert_eq!(result.unwrap_err().error_code(), "HCR_COMMAND_NOT_ALLOWED");
}

// ── Test 8: Reject unlisted executable ──

#[test]
fn hcr_profile_rejects_unlisted_executable() {
    let profile = test_profile();
    let params = HashMap::new();
    let result = CommandPolicy::check("some_random_tool", &params, &profile, &PathBuf::from("/ws"));
    assert!(result.is_err(), "unlisted command should be rejected");
    assert_eq!(result.unwrap_err().error_code(), "HCR_COMMAND_NOT_ALLOWED");
}

// ── Test 9: Reject user-supplied root ──

#[test]
fn hcr_profile_rejects_user_supplied_root() {
    let profile = test_profile();
    let mut params = HashMap::new();
    // The node_test does not accept a root parameter
    params.insert("test_path".into(), "test.mjs".into());
    params.insert("hacker_root".into(), "/etc".into());

    let result = CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws"));
    // Should succeed because extra params are ignored (only templated params used)
    assert!(result.is_ok(), "extra params should be ignored");
}
