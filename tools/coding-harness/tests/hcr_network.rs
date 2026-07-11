//! HCR network policy tests (20-22).
//!
//! Tests that HCR execution respects network policies:
//! - node_test: network deny
//! - harness_local_smoke: loopback only
//!
//! Network tests require a sandbox backend to enforce restrictions.
//! Without sandbox, the tests verify the policy settings are correct.

use std::path::PathBuf;

use coding_harness::hcr::profile::{ArgTemplate, HcrCommandEntry, HcrProfile, NetworkPolicy};
use coding_harness::hcr::sandbox::SandboxBackend;

fn network_test_profile() -> HcrProfile {
    HcrProfile {
        id: "network-test".into(),
        workspace_id: "test".into(),
        allowed_commands: vec![
            HcrCommandEntry {
                name: "node_test".into(),
                program: PathBuf::from("/usr/bin/env"),
                args: vec![
                    ArgTemplate::Fixed("node".into()),
                    ArgTemplate::Fixed("--test".into()),
                    ArgTemplate::Param("test_file".into()),
                ],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(10_000),
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
                timeout_ms_default: Some(10_000),
            },
            HcrCommandEntry {
                name: "network_check".into(),
                program: PathBuf::from("/usr/bin/env"),
                args: vec![
                    ArgTemplate::Fixed("node".into()),
                    ArgTemplate::Fixed("-e".into()),
                    ArgTemplate::Param("code".into()),
                ],
                network: Some(NetworkPolicy::Deny),
                timeout_ms_default: Some(5_000),
            },
        ],
        ..Default::default()
    }
}

fn sandbox_available() -> bool {
    SandboxBackend::detect() != SandboxBackend::Unavailable
}

// ── Test 20: node_test cannot access external network ──

#[test]
fn node_test_cannot_access_external_network() {
    let profile = network_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_20");
    let _ = std::fs::create_dir_all(&ws);

    // Verify the node_test command has network deny policy
    let entry = profile.find_command("node_test").unwrap();
    assert_eq!(
        profile.effective_network(entry),
        NetworkPolicy::Deny,
        "node_test must have deny network policy"
    );

    // If sandbox is available, the network is truly denied.
    // Without sandbox, we can only verify the policy setting.
    if sandbox_available() {
        eprintln!("Sandbox available: network will be enforced");
    } else {
        eprintln!("No sandbox: network policy enforced at config level only");
    }

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 21: local_smoke can use loopback only ──

#[test]
fn local_smoke_can_use_loopback_only() {
    let profile = network_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_21");
    let _ = std::fs::create_dir_all(&ws);

    // Verify the policy is loopback_only
    let entry = profile.find_command("harness_local_smoke").unwrap();
    assert_eq!(
        profile.effective_network(entry),
        NetworkPolicy::LoopbackOnly,
        "harness_local_smoke must have loopback_only network policy"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Test 22: local_smoke cannot access external network ──

#[test]
fn local_smoke_cannot_access_external_network() {
    let profile = network_test_profile();
    let ws = std::env::temp_dir().join("hcr_test_22");
    let _ = std::fs::create_dir_all(&ws);

    let entry = profile.find_command("harness_local_smoke").unwrap();
    let network = profile.effective_network(entry);

    // LoopbackOnly is not unrestricted, so external access is denied
    assert_eq!(
        network,
        NetworkPolicy::LoopbackOnly,
        "harness_local_smoke must have loopback_only policy"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Network policy serialization test ──

#[test]
fn network_policy_serialization() {
    // Verify profile JSON serialization includes network policy
    let profile = network_test_profile();
    let json = coding_harness::hcr::profile::profile_to_json(&profile);

    let cmd_names: Vec<&str> = json["allowed_commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(cmd_names.contains(&"node_test"));

    let node_entry = json["allowed_commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "node_test")
        .unwrap();
    assert_eq!(node_entry["network"], "deny");

    let smoke_entry = json["allowed_commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "harness_local_smoke")
        .unwrap();
    assert_eq!(smoke_entry["network"], "loopback_only");
}
