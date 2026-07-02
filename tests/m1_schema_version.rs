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
        4,
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
    assert_eq!(journal.schema_version()?, 4);
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A database newer than the kernel must be rejected at startup.
#[test]
fn newer_schema_version_is_rejected_cleanly() -> Result<()> {
    let db_path = unique_temp_path();
    // Pre-stamp as version 5 (newer than kernel's CURRENT_SCHEMA_VERSION of 4).
    {
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
        conn.pragma_update(None, "user_version", 5)?;
    }
    // Opening with the kernel (whose CURRENT_SCHEMA_VERSION is 4) must fail.
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
