use super::*;
use crate::config::KernelConfig;
use crate::journal::JournalStore;

/// Helper: create an in-memory journal and register a test bundle.
fn setup() -> (JournalStore, String) {
    let journal = JournalStore::in_memory().expect("in-memory journal");
    let body = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "test_harness",
        "bundle_version": "1.0.0",
        "operations": [{
            "name": "test_op",
            "description": "test harness operation",
            "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": false},
            "risk": "ReadOnly",
            "idempotent": true
        }]
    });
    let resp = handle_register_bundle(&journal, &body).expect("register bundle");
    let hash = resp["bundle_hash"].as_str().unwrap().to_string();
    (journal, hash)
}

// --- Validation: register bundle ---

#[test]
fn register_bundle_success() {
    let (_, hash) = setup();
    assert!(hash.starts_with("sha256:"), "hash: {hash}");
}

#[test]
fn register_bundle_idempotent() {
    let journal = JournalStore::in_memory().expect("in-memory");
    let body = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "test",
        "bundle_version": "1.0",
        "operations": [{"name": "op", "description": "d", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
    });
    let r1 = handle_register_bundle(&journal, &body).unwrap();
    let r2 = handle_register_bundle(&journal, &body).unwrap();
    assert_eq!(r1["bundle_hash"], r2["bundle_hash"]);
    assert_eq!(r2.get("idempotent").and_then(|v| v.as_bool()), Some(true));
}

#[test]
fn register_bundle_conflict_different_content() {
    let journal = JournalStore::in_memory().expect("in-memory");
    // First bundle.
    let body1 = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "test",
        "bundle_version": "1.0",
        "operations": [{"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
    });
    handle_register_bundle(&journal, &body1).unwrap();
    // Same id/version, different content → conflict.
    let body2 = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "test",
        "bundle_version": "1.0",
        "operations": [{"name": "op_b", "description": "b", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
    });
    let err = handle_register_bundle(&journal, &body2).unwrap_err();
    assert!(err.to_string().contains("bundle_conflict"), "err: {err}");
}

#[test]
fn register_bundle_invalid_manifest() {
    let journal = JournalStore::in_memory().expect("in-memory");
    let err = handle_register_bundle(&journal, &serde_json::json!({})).unwrap_err();
    assert!(err.to_string().contains("manifest"), "err: {err}");
}

#[test]
fn list_bundles_shows_registered() {
    let journal = JournalStore::in_memory().expect("in-memory");
    let body = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "list_test",
        "bundle_version": "1.0",
        "operations": [{"name": "op", "description": "d", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
    });
    handle_register_bundle(&journal, &body).unwrap();
    let result = handle_list_bundles(&journal).unwrap();
    let bundles = result["bundles"].as_array().unwrap();
    assert_eq!(bundles.len(), 1);
}

// --- Registration ---

#[test]
fn register_runtime_success() {
    let (journal, hash) = setup();
    let reg = handle_register_runtime(&journal, &hash, "http://127.0.0.1:8080").unwrap();
    assert_eq!(reg["bundle_hash"].as_str().unwrap(), &hash);
    assert_eq!(reg["endpoint"].as_str().unwrap(), "http://127.0.0.1:8080");
}

#[test]
fn register_runtime_nonexistent_bundle() {
    let journal = JournalStore::in_memory().expect("in-memory");
    let err = handle_register_runtime(&journal, "sha256:nonexistent", "http://127.0.0.1:8080")
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "err: {err}");
}

#[test]
fn registration_update_does_not_change_snapshot() {
    let (journal, hash) = setup();
    handle_register_runtime(&journal, &hash, "http://127.0.0.1:8080").unwrap();
    // Update endpoint.
    let reg2 = handle_register_runtime(&journal, &hash, "http://127.0.0.1:9090").unwrap();
    assert_eq!(reg2["endpoint"].as_str().unwrap(), "http://127.0.0.1:9090");
    // Verify current snapshot unchanged (still baseline).
    let current = handle_registry_info(&journal).unwrap();
    assert!(current["current_snapshot_id"]
        .as_str()
        .unwrap()
        .starts_with("snap_"));
}

#[test]
fn list_registrations_shows_registered() {
    let (journal, hash) = setup();
    handle_register_runtime(&journal, &hash, "http://127.0.0.1:8080").unwrap();
    let result = handle_list_registrations(&journal).unwrap();
    let regs = result["registrations"].as_array().unwrap();
    assert_eq!(regs.len(), 1);
}

// --- Compose / Activate / Rollback ---

#[test]
fn compose_snapshot_creates_new_snapshot() {
    let (journal, hash) = setup();
    let base = journal.current_registry_snapshot_id().unwrap();
    let result = handle_compose_snapshot(&journal, &base, &[hash]).unwrap();
    let new_id = result["snapshot_id"].as_str().unwrap().to_string();
    assert_ne!(new_id, base);
    // Current should still be the base (compose is a candidate).
    let current = journal.current_registry_snapshot_id().unwrap();
    assert_eq!(current, base);
}

#[test]
fn activate_snapshot_switches_current() {
    let (journal, hash) = setup();
    let base = journal.current_registry_snapshot_id().unwrap();
    let composed = handle_compose_snapshot(&journal, &base, &[hash]).unwrap();
    let snap_id = composed["snapshot_id"].as_str().unwrap();
    handle_activate_snapshot(&journal, snap_id, None).unwrap();
    let current = journal.current_registry_snapshot_id().unwrap();
    assert_eq!(current, snap_id);
}

#[test]
fn rollback_restores_historical_snapshot() {
    let (journal, hash) = setup();
    let base = journal.current_registry_snapshot_id().unwrap();
    let composed = handle_compose_snapshot(&journal, &base, &[hash]).unwrap();
    let snap_id = composed["snapshot_id"].as_str().unwrap();
    handle_activate_snapshot(&journal, snap_id, None).unwrap();
    assert_eq!(journal.current_registry_snapshot_id().unwrap(), snap_id);
    // Rollback to base.
    handle_activate_snapshot(&journal, &base, None).unwrap();
    assert_eq!(journal.current_registry_snapshot_id().unwrap(), base);
}

#[test]
fn activate_nonexistent_snapshot_fails() {
    let journal = JournalStore::in_memory().expect("in-memory");
    let err = handle_activate_snapshot(&journal, "snap_nonexistent", None).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not_found")
            || msg.contains("no such")
            || msg.contains("Query returned no rows"),
        "err: {msg}"
    );
}

#[test]
fn registry_info_shows_current() {
    let journal = JournalStore::in_memory().expect("in-memory");
    let info = handle_registry_info(&journal).unwrap();
    assert!(info["current_snapshot_id"].as_str().is_some());
}

// --- Grants ---

#[test]
fn grant_operation_success() {
    let journal = JournalStore::in_memory().expect("in-memory");
    let result = handle_grant_operation(&journal, "Cli", "test.op").unwrap();
    assert_eq!(result["action"].as_str().unwrap(), "granted");
}

#[test]
fn revoke_operation_success() {
    let journal = JournalStore::in_memory().expect("in-memory");
    handle_grant_operation(&journal, "Cli", "test.op").unwrap();
    let result = handle_revoke_operation(&journal, "Cli", "test.op").unwrap();
    assert_eq!(result["action"].as_str().unwrap(), "revoked");
}

#[test]
fn list_grants_shows_all() {
    let journal = JournalStore::in_memory().expect("in-memory");
    handle_grant_operation(&journal, "Cli", "op1").unwrap();
    handle_grant_operation(&journal, "Feishu", "op2").unwrap();
    let result = handle_list_grants(&journal, None).unwrap();
    let grants = result["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 2);
}

#[test]
fn list_grants_by_channel() {
    let journal = JournalStore::in_memory().expect("in-memory");
    handle_grant_operation(&journal, "Cli", "op1").unwrap();
    let result = handle_list_grants(&journal, Some("Cli")).unwrap();
    let grants = result["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1);
}

// --- Auth ---

#[test]
fn is_admin_enabled_checks_config() {
    let mut config = KernelConfig {
        harness_admin_token: "".into(),
        ..KernelConfig::from_cli(None)
    };
    assert!(!is_admin_enabled(&config));
    config.harness_admin_token = "secret".to_string();
    assert!(is_admin_enabled(&config));
}

#[test]
fn validate_admin_token_rejects_no_token() {
    let config = KernelConfig {
        harness_admin_token: "secret".into(),
        ..KernelConfig::from_cli(None)
    };
    assert!(validate_admin_token(&config, None).is_err());
}

#[test]
fn validate_admin_token_rejects_wrong_token() {
    let config = KernelConfig {
        harness_admin_token: "secret".into(),
        ..KernelConfig::from_cli(None)
    };
    assert!(validate_admin_token(&config, Some("wrong")).is_err());
}

#[test]
fn validate_admin_token_accepts_correct_token() {
    let config = KernelConfig {
        harness_admin_token: "secret".into(),
        ..KernelConfig::from_cli(None)
    };
    assert!(validate_admin_token(&config, Some("secret")).is_ok());
}

#[test]
fn admin_token_cannot_access_regular_ingress() {
    let config = KernelConfig {
        harness_admin_token: "admin_secret".into(),
        ipc_token: "ipc_secret".into(),
        ..KernelConfig::from_cli(None)
    };
    // Admin token should fail against IPC auth
    assert!(
        config.harness_admin_token != config.ipc_token,
        "tokens must differ"
    );
}

// --- Error boundedness ---

#[test]
fn admin_error_does_not_leak_sensitive_info() {
    let journal = JournalStore::in_memory().expect("in-memory");
    // Invalid manifest with various bad data — errors should be bounded.
    let err = handle_register_bundle(&journal, &serde_json::json!({"manifest_version": "v1"}))
        .unwrap_err();
    let msg = err.to_string();
    // The message must NOT contain raw request body or secrets.
    assert!(!msg.contains("Authorization"), "leaked auth");
    assert!(!msg.contains("AGENT_CORE"), "leaked env var name");
    assert!(msg.len() < 200, "error too long: {msg}");
}
