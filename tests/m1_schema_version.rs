use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;

/// A fresh database is migrated and stamped with the current schema version.
/// (Phase 1 hardening: migration check.)
#[test]
fn fresh_database_is_stamped_with_current_schema_version() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert_eq!(
        journal.schema_version()?,
        2,
        "a fresh database must be stamped with the current schema version"
    );
    Ok(())
}

/// Re-opening an existing at-version database succeeds and keeps the version
/// stamp. The base migration is not re-run (idempotency), but the projection
/// / dedup heals still run.
#[test]
fn existing_at_version_database_reopens_cleanly() -> Result<()> {
    let db_path = unique_temp_path();

    // First open: creates + migrates + stamps version 2.
    {
        let _journal = JournalStore::open(&db_path)?;
    }
    // Second open on the same file: must succeed and keep the stamp.
    let journal = JournalStore::open(&db_path)?;
    assert_eq!(journal.schema_version()?, 2);
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A database whose `user_version` is NEWER than the kernel understands must
/// be rejected at startup with a clear, sanitized message — not silently
/// re-migrated. This protects against an older kernel corrupting a newer
/// schema.
#[test]
fn newer_schema_version_is_rejected_cleanly() -> Result<()> {
    let db_path = unique_temp_path();

    // Pre-stamp the database as version 3 (newer than the kernel's
    // CURRENT_SCHEMA_VERSION of 2) using a raw connection.
    {
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
        conn.pragma_update(None, "user_version", 3)?;
    }

    // Opening with the kernel (whose CURRENT_SCHEMA_VERSION is 2) must fail.
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
