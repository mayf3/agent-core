//! Targeted tests for manifest idempotent reuse: created_at and
//! canonical_digest must participate in strict structural comparison.

use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use serde_json::json;

fn valid_manifest() -> HarnessManifest {
    HarnessManifest {
        manifest_id: String::new(),
        harness_id: "idem_test".into(),
        artifact_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:9999/test".into(),
        operation_name: "external.idem_test".into(),
        description: "idempotent test".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
        idempotent: true,
        created_at: Utc::now(),
    }
}

fn registered_manifest(j: &JournalStore) -> Result<HarnessManifest> {
    let mut m = valid_manifest();
    m.manifest_id = m.compute_manifest_id()?;
    j.register_harness_manifest(&m)?;
    Ok(m)
}

#[test]
fn identical_manifest_is_idempotently_reused() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let m1 = registered_manifest(&j)?;
    // Register the exact same manifest again.
    let id2 = j.register_harness_manifest(&m1)?;
    assert_eq!(
        id2, m1.manifest_id,
        "same manifest must return same manifest_id"
    );
    Ok(())
}

#[test]
fn same_manifest_id_with_different_created_at_is_rejected() -> Result<()> {
    let j = JournalStore::in_memory()?;

    // Register the initial manifest through the normal public path.
    let created_at_1 = Utc::now();
    let mut m1 = valid_manifest();
    m1.created_at = created_at_1;
    m1.manifest_id = m1.compute_manifest_id()?;
    j.register_harness_manifest(&m1)?;

    // Build a manifest with the same manifest_id but different created_at.
    // Use the in-tx method directly since the public wrapper's manifest_id
    // validation would reject the mismatch before reaching the DB.
    let created_at_2 = created_at_1 + chrono::Duration::seconds(1);
    let mut m2 = m1.clone();
    m2.created_at = created_at_2;

    let mut conn = j.conn.lock().unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let err = j
        .register_harness_manifest_in_tx(&tx, &m2)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("manifest_identity_conflict"),
        "expected manifest_identity_conflict; got: {err}"
    );
    drop(tx);
    drop(conn);
    Ok(())
}

#[test]
fn corrupted_persisted_canonical_digest_is_rejected() -> Result<()> {
    // Register a manifest, then corrupt its canonical_digest in the DB.
    let j = JournalStore::in_memory()?;
    let m1 = registered_manifest(&j)?;
    let conn = j.conn.lock().unwrap();
    conn.execute(
        "UPDATE harness_manifests SET canonical_digest = 'sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff' WHERE manifest_id = ?1",
        params![m1.manifest_id],
    )?;
    drop(conn);

    let err = j.register_harness_manifest(&m1).unwrap_err().to_string();
    assert!(
        err.contains("corrupt_persisted_canonical_digest")
            || err.contains("already registered with different content"),
        "got: {err}"
    );
    Ok(())
}

#[test]
fn corrupted_persisted_schema_is_rejected() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let m1 = registered_manifest(&j)?;
    let conn = j.conn.lock().unwrap();
    // Corrupt both the schema AND canonical_digest so the public wrapper
    // does not short-circuit at the digest check and the _in_tx function
    // catches the schema corruption.
    conn.execute(
        "UPDATE harness_manifests SET input_schema_json = 'not valid json', canonical_digest = 'sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff' WHERE manifest_id = ?1",
        params![m1.manifest_id],
    )?;
    drop(conn);

    let err = j.register_harness_manifest(&m1).unwrap_err().to_string();
    assert!(
        err.contains("corrupt_persisted")
            || err.contains("already registered with different content"),
        "got: {err}"
    );
    Ok(())
}

// ── register_harness_manifest_replace_tx tests ─────────────────────────

#[test]
fn replace_tx_idempotent_same_manifest_id_and_digest() -> Result<()> {
    // Call register_harness_manifest_replace_tx twice with the same manifest.
    // The second call must idempotently return without a PRIMARY KEY conflict.
    let j = JournalStore::in_memory()?;
    let mut m = valid_manifest();
    m.manifest_id = m.compute_manifest_id()?;

    let mut conn = j.conn.lock().unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // First call: manifest not yet in DB → INSERT.
    let id1 = j.register_harness_manifest_replace_tx(&tx, &m)?;
    assert_eq!(id1, m.manifest_id, "first call must return manifest_id");

    // Second call: manifest already exists, canonical_digest matches → idempotent return.
    let id2 = j.register_harness_manifest_replace_tx(&tx, &m)?;
    assert_eq!(
        id2, m.manifest_id,
        "second call must idempotently return same manifest_id"
    );

    drop(tx);
    drop(conn);
    Ok(())
}

#[test]
fn replace_tx_rejects_canonical_digest_mismatch() -> Result<()> {
    // Insert a manifest via register_harness_manifest_replace_tx, then
    // corrupt its canonical_digest in the DB. A second call with the same
    // manifest must bail with manifest_reuse_conflict (no silent overwrite).
    let j = JournalStore::in_memory()?;
    let mut m = valid_manifest();
    m.manifest_id = m.compute_manifest_id()?;

    let mut conn = j.conn.lock().unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // First call: INSERT the manifest.
    j.register_harness_manifest_replace_tx(&tx, &m)?;

    // Corrupt the stored canonical_digest to simulate same manifest_id,
    // different content.
    tx.execute(
        "UPDATE harness_manifests SET canonical_digest = 'sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff' WHERE manifest_id = ?1",
        params![m.manifest_id],
    )?;

    // Second call: canonical_digest mismatch must be rejected.
    let err = j
        .register_harness_manifest_replace_tx(&tx, &m)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("manifest_reuse_conflict"),
        "expected manifest_reuse_conflict; got: {err}"
    );

    drop(tx);
    drop(conn);
    Ok(())
}

#[test]
fn manifest_lookup_error_is_not_treated_as_not_found() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let m1 = registered_manifest(&j)?;
    // Drop the harness_manifests table so query_row returns a real SQLite error.
    let conn = j.conn.lock().unwrap();
    conn.execute("DROP TABLE harness_manifests", [])?;
    drop(conn);

    let err = j.register_harness_manifest(&m1).unwrap_err().to_string();
    // The error must be about the DB error, not "not_found" or a panic.
    assert!(
        !err.contains("not_found"),
        "lookup error must not be reported as not_found; got: {err}"
    );
    assert!(
        err.contains("manifest_lookup_failed") || err.contains("no such table"),
        "got: {err}"
    );
    Ok(())
}

#[test]
fn builtin_bootstrap_appends_schema_upgrade_and_preserves_old_manifest() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let operation = crate::domain::operation::external::TASK_SUBMIT;
    let endpoint = "http://127.0.0.1:7200";
    let artifact_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    let mut old = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "coding-harness-v0".into(),
        artifact_digest: artifact_digest.into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: endpoint.into(),
        operation_name: operation.into(),
        description: "old builtin submit schema".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"development_request": {"type": "object"}},
            "required": ["development_request"],
            "additionalProperties": false
        }),
        output_schema: json!({"type": "object"}),
        idempotent: true,
        created_at: Utc::now(),
    };
    old.manifest_id = old.compute_manifest_id()?;
    j.register_harness_manifest(&old)?;

    let first = j.bootstrap_builtin_external_manifests(endpoint, artifact_digest)?;
    let upgraded_id = first.get(operation).unwrap();
    assert_ne!(upgraded_id, &old.manifest_id);
    assert!(j.load_harness_manifest(&old.manifest_id)?.is_some());
    assert!(j.load_harness_manifest(upgraded_id)?.is_some());

    let count_after_upgrade: i64 = j.conn.lock().unwrap().query_row(
        "SELECT COUNT(*) FROM harness_manifests WHERE operation_name = ?1",
        params![operation],
        |row| row.get(0),
    )?;
    assert_eq!(count_after_upgrade, 2);

    let replay = j.bootstrap_builtin_external_manifests(endpoint, artifact_digest)?;
    assert_eq!(replay.get(operation), Some(upgraded_id));
    let count_after_replay: i64 = j.conn.lock().unwrap().query_row(
        "SELECT COUNT(*) FROM harness_manifests WHERE operation_name = ?1",
        params![operation],
        |row| row.get(0),
    )?;
    assert_eq!(count_after_replay, count_after_upgrade);
    Ok(())
}
