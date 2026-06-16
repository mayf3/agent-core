use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;

/// Phase 1 user-facing goal: "shutdown / restart behavior is predictable".
///
/// This test exercises the full restart-recovery lifecycle against a file DB:
/// 1. A dispatch is started and then abandoned mid-flight (simulate a crash
///    after `DispatchStarted` with no terminal fact).
/// 2. The process "restarts": a fresh `JournalStore::open` reopens the same DB
///    file. Before recovery, health is degraded (the in-flight dispatch has no
///    terminal outcome).
/// 3. Recovery runs and reconciles the abandoned dispatch to a terminal
///    `unknown` state, appending `OutboxDispatchUnknown` as the source of
///    truth. After recovery, the unknown count is exposed in health.
/// 4. Re-running recovery is idempotent (no duplicate `OutboxDispatchUnknown`).
///
/// Crucially the abandoned dispatch is NEVER auto-retried — the invariant the
/// operator relies on for predictable restart behavior.
#[test]
fn restart_recovery_reconciles_abandoned_dispatch_and_never_redispatches() -> Result<()> {
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let db_path = unique_temp_path();

    // --- Phase 1: a dispatch starts, then the process "crashes". ---
    {
        let journal = JournalStore::open(&db_path)?;
        let session = common::test_session(&config);
        let run = common::test_run(&config, &session);
        let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
        journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
        journal.start_outbox_dispatch(&approved, Some(&session.id))?;
        // "crash" — the journal is dropped without completing the dispatch.
    }

    // --- Phase 2: "restart" — reopen the same DB file. ---
    let journal = JournalStore::open(&db_path)?;

    // Before recovery, the abandoned dispatch is in-flight with no terminal
    // fact, so it shows up in unknown_invocations (health degraded).
    let before = health_snapshot(&journal, true, &DispatcherMetrics::new())?;
    assert_eq!(
        before.get("status").and_then(|v| v.as_str()),
        Some("degraded"),
        "an abandoned in-flight dispatch must degrade health before recovery"
    );
    let unknown_before = before
        .get("unknown_invocation_count")
        .and_then(|v| v.as_u64());
    assert!(
        unknown_before.unwrap_or(0) >= 1,
        "the abandoned dispatch must be counted as an unknown invocation"
    );

    // --- Phase 3: recovery runs. ---
    let recovered = journal.recover_unknown_invocations()?;
    assert_eq!(recovered, 1, "exactly one abandoned dispatch is recovered");

    // The dispatch is now terminal `unknown` — never retried.
    let events = journal.events()?;
    let unknown_facts = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxDispatchUnknown)
        .count();
    assert_eq!(
        unknown_facts, 1,
        "recovery appends exactly one OutboxDispatchUnknown terminal fact"
    );
    assert_eq!(
        journal.outbox_status_count(OutboxDispatchStatus::Unknown)?,
        1,
        "the projection is reconciled to unknown"
    );
    assert_eq!(
        journal.outbox_status_count(OutboxDispatchStatus::Dispatching)?,
        0,
        "no row is left dispatching after recovery"
    );

    // --- Phase 4: recovery is idempotent. ---
    let recovered_again = journal.recover_unknown_invocations()?;
    assert_eq!(
        recovered_again, 0,
        "a second recovery must not reprocess an already-terminal dispatch"
    );
    let unknown_facts_again = journal
        .events()?
        .iter()
        .filter(|e| e.kind == JournalEventKind::OutboxDispatchUnknown)
        .count();
    assert_eq!(
        unknown_facts_again, 1,
        "no duplicate OutboxDispatchUnknown fact on re-recovery"
    );

    // Hash chain must remain intact across the restart + recovery.
    assert!(
        journal.verify_hash_chain()?,
        "hash chain must be intact after restart + recovery"
    );

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A unique .db path directly under the OS temp dir (no wrapper dir, which
/// avoids SQLite's bundled "database file has moved" quirk on re-open).
fn unique_temp_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "agent-core-restart-{}-{}.db",
        std::process::id(),
        n
    ))
}

#[path = "common/mod.rs"]
mod common;
