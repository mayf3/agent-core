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
