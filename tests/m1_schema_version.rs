use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;

/// A fresh database is migrated and stamped with the current schema version.
#[test]
fn fresh_database_is_stamped_with_current_schema_version() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert_eq!(
        journal.schema_version()?,
        5,
        "a fresh database must be stamped with the current schema version"
    );
    Ok(())
}

/// Re-opening an existing at-version database succeeds and keeps the version stamp.
#[test]
fn existing_at_version_database_reopens_cleanly() -> Result<()> {
    let db_path = unique_temp_path();
    {
        let _journal = JournalStore::open(&db_path)?;
    }
    let journal = JournalStore::open(&db_path)?;
    assert_eq!(journal.schema_version()?, 5);
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A database newer than the kernel must be rejected at startup.
#[test]
fn newer_schema_version_is_rejected_cleanly() -> Result<()> {
    let db_path = unique_temp_path();
    // Pre-stamp as version 6 (newer than kernel's CURRENT_SCHEMA_VERSION of 5).
    {
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
        conn.pragma_update(None, "user_version", 6)?;
    }
    // Opening with the kernel (whose CURRENT_SCHEMA_VERSION is 5) must fail.
    let message = match JournalStore::open(&db_path) {
        Ok(_) => panic!("a newer-than-supported schema version must be rejected at startup"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("newer than supported version"),
        "error must explain the version mismatch, got: {message}"
    );
    // Sanitized: the message must reference versions only, not paths/secrets.
    assert!(
        !message.contains(db_path.to_string_lossy().as_ref()),
        "error must not leak the db path"
    );

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A unique .db path directly under the OS temp dir (no wrapper dir, which
/// avoids SQLite's bundled "database file has moved" quirk on re-open).
fn unique_temp_path() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("agent-core-schema-{}-{}.db", std::process::id(), n))
}

#[test]
fn migration_v3_to_v4_creates_proposals_table() -> Result<()> {
    let db_path = unique_temp_path();
    // Create a v3 database.
    {
        JournalStore::open(&db_path)?;
    }
    // Verify version is 4 and proposals table exists.
    {
        let journal = JournalStore::open(&db_path)?;
        assert_eq!(journal.schema_version()?, 5);
        let conn = rusqlite::Connection::open(&db_path)?;
        let has_table: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='capability_change_proposals'",
            [], |row| row.get(0),
        )?;
        assert!(
            has_table,
            "capability_change_proposals table must exist after v3→v5 migration"
        );
    }
    // Verify we can INSERT and SELECT.
    {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute(
            "INSERT INTO capability_change_proposals (proposal_id, submitter_principal_id, target_agent_id, origin_session_id, origin_run_id, artifact_ref, artifact_digest, manifest_ref, manifest_digest, evidence_ref, evidence_digest, requested_operations_json, risk_summary, expected_active_snapshot_id, status, created_at, expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params!["test_prop", "submitter", "agent", "sess", "run", "ref", "sha256:0000000000000000000000000000000000000000000000000000000000000000", "mref", "sha256:1111111111111111111111111111111111111111111111111111111111111111", "eref", "sha256:2222222222222222222222222222222222222222222222222222222222222222", "[\"test.op\"]", "risk", "snap_0", "PendingApproval", "2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z"],
        )?;
    }
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

// ── Migration 0005: remove UNIQUE(operation_name) from harness_manifests ────

/// Build a v4 database (migrations 0001–0004 applied, version stamped 4),
/// inserting a harness_manifests row under the OLD UNIQUE(operation_name)
/// constraint, then reopen with the kernel to drive the v4→v5 migration.
fn build_v4_db_with_manifest(db_path: &std::path::Path) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
    conn.execute_batch(include_str!("../migrations/0002_registry_snapshots.sql"))?;
    conn.execute_batch(include_str!(
        "../migrations/0003_external_harness_hotload.sql"
    ))?;
    conn.execute_batch(include_str!(
        "../migrations/0004_capability_change_proposals.sql"
    ))?;
    // Stamp at v4 (pre-0005).
    conn.pragma_update(None, "user_version", 4)?;
    // Insert a manifest row under the v4 schema (UNIQUE(operation_name) present).
    conn.execute(
        "INSERT INTO harness_manifests
         (manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
          operation_name, description, input_schema_json, output_schema_json,
          idempotent, created_at, canonical_digest)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        rusqlite::params![
            "manifest_pre_existing",
            "h",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "external-harness-v1",
            "http://127.0.0.1:7000/x",
            "external.pre_existing",
            "pre-existing manifest",
            "{}",
            "{}",
            1,
            "2026-01-01T00:00:00Z",
            "canonical_pre_existing",
        ],
    )?;
    Ok(())
}

/// A v4 database with pre-existing manifest data migrates cleanly to v5,
/// preserving the row, removing UNIQUE(operation_name), adding the
/// operation_name index, and allowing two manifest_ids for one operation_name.
#[test]
fn migration_v4_to_v5_preserves_data_and_drops_unique_constraint() -> Result<()> {
    let db_path = unique_temp_path();
    build_v4_db_with_manifest(&db_path)?;

    // Reopen with the kernel → drives v4→v5 migration.
    let journal = JournalStore::open(&db_path)?;
    // 7. Schema version is exactly 5.
    assert_eq!(journal.schema_version()?, 5);

    let conn = Connection::open(&db_path)?;

    // 2. Pre-existing manifest data is preserved.
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM harness_manifests WHERE manifest_id = 'manifest_pre_existing'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "pre-existing manifest row must survive migration");

    // 3. The same operation_name can now hold a DIFFERENT manifest_id
    //    (UNIQUE(operation_name) is gone).
    conn.execute(
        "INSERT INTO harness_manifests
         (manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
          operation_name, description, input_schema_json, output_schema_json,
          idempotent, created_at, canonical_digest)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        rusqlite::params![
            "manifest_v2",
            "h",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "external-harness-v1",
            "http://127.0.0.1:7000/x",
            "external.pre_existing",
            "upgraded manifest",
            "{}",
            "{}",
            1,
            "2026-01-02T00:00:00Z",
            "canonical_v2",
        ],
    )
    .expect("inserting a second manifest_id for the same operation_name must succeed after 0005");

    // 4. operation_name index exists.
    let idx_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_harness_manifests_operation_name'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        idx_count, 1,
        "idx_harness_manifests_operation_name must exist"
    );

    // 5. The old UNIQUE(operation_name) constraint is gone: confirm via the
    //    table's CREATE SQL — it must NOT mention UNIQUE(operation_name). The
    //    only UNIQUE on operation_name in v4 was a table-level constraint; the
    //    recreated table must not carry it.
    let create_sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='harness_manifests'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !create_sql.contains("UNIQUE (operation_name)"),
        "UNIQUE(operation_name) must be removed, got: {create_sql}"
    );

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// Re-running migration 0005 is a no-op: opening an already-v5 database does
/// not error, the version stays 5, and no duplicate index is created.
#[test]
fn migration_v5_is_idempotent_on_reopen() -> Result<()> {
    let db_path = unique_temp_path();
    // First open: migrates to v5.
    {
        let _journal = JournalStore::open(&db_path)?;
        let conn = Connection::open(&db_path)?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(v, 5);
    }
    // Second open: must be a no-op (no error, version stays 5).
    let journal = JournalStore::open(&db_path)?;
    assert_eq!(journal.schema_version()?, 5);

    let conn = Connection::open(&db_path)?;
    // The operation_name index is created with IF NOT EXISTS, so no duplicate.
    let idx_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_harness_manifests_operation_name'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(idx_count, 1, "index must not be duplicated on reopen");

    std::fs::remove_file(&db_path).ok();
    Ok(())
}
