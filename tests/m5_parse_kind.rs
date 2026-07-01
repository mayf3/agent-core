use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

/// Regression for the `parse_kind` fallback tightening (HANDOVER §10).
///
/// An unrecognized `kind` string must route to the `Unknown` sentinel, not be
/// silently coerced to `RunCompleted`. This test corrupts the kind column of
/// a real event and asserts (a) `events()` still succeeds and reports
/// `Unknown`, and (b) `verify_hash_chain` still flags the row as corrupt
/// (re-serialized `"Unknown"` != stored garbage), preserving integrity
/// semantics.
#[test]
fn unrecognized_kind_routes_to_unknown_and_keeps_chain_corrupt() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    journal.append_event(
        JournalEventKind::RunCompleted,
        None,
        None,
        Some("evt_1"),
        json!({}),
    )?;
    journal.tamper_first_event_kind_for_test("GarbageKind")?;

    let events = journal.events()?;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind,
        JournalEventKind::Unknown,
        "unrecognized kind must route to Unknown, not RunCompleted"
    );
    assert!(
        !journal.verify_hash_chain()?,
        "hash chain must remain flagged corrupt when a kind was tampered"
    );
    Ok(())
}

/// The core behavioral fix: an unrecognized kind must NOT be treated as a
/// delivered-set kind by `undelivered_ingress_events`. Before the fix,
/// corrupting a SessionReady row's kind to garbage still left the correlated
/// ingress "delivered" because the garbage parsed to RunCompleted.
#[test]
fn unknown_kind_is_not_treated_as_delivered() -> Result<()> {
    let journal = JournalStore::in_memory()?;

    // Append SessionReady FIRST so it is sequence 1 (the row our tamper
    // helper targets), sharing the ingress event_id as correlation_id.
    journal.append_event(
        JournalEventKind::SessionReady,
        None,
        None,
        Some("evt_2"),
        json!({ "session_id": "session_test" }),
    )?;
    journal.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some("evt_2"),
        json!({ "event_id": "evt_2" }),
    )?;
    assert!(
        journal.undelivered_ingress_events()?.is_empty(),
        "SessionReady should mark the ingress as delivered before tampering"
    );

    // Corrupt the SessionReady (sequence 1) kind so it is no longer
    // recognized. After the fix it routes to Unknown, which does not match
    // the delivered-set predicate, so the ingress event reappears.
    journal.tamper_first_event_kind_for_test("FutureKind")?;

    let after = journal.undelivered_ingress_events()?;
    assert_eq!(
        after.len(),
        1,
        "unknown kind must not count as delivered; ingress should reappear"
    );
    Ok(())
}

/// §8: persistence + read-back for the new tool-call Journal kinds
/// (`ToolCallIssued`, `ToolCallRejected`). These kinds are NEW in PR #155; the
/// read path (`parse_kind`) and the hash chain must survive a real SQLite
/// close + reopen so an operator restarting the kernel can read the full audit
/// trail and verify integrity.
#[test]
fn tool_call_kinds_survive_close_reopen_with_intact_hash_chain() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "m5-tool-call-kinds-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("tool_call_kinds.db");
    let run = RunId("run_persist_test".to_string());
    let session = SessionId("session_persist_test".to_string());

    {
        let journal = JournalStore::open(&db_path)?;
        journal.append_event(
            JournalEventKind::RunStarted,
            Some(&run),
            Some(&session),
            None,
            json!({ "run_id": run.0 }),
        )?;
        journal.append_event(
            JournalEventKind::ToolCallIssued,
            Some(&run),
            Some(&session),
            None,
            json!({ "operation": "system.status", "tool_call_id": "hashed_id_1" }),
        )?;
        journal.append_event(
            JournalEventKind::ToolCallRejected,
            Some(&run),
            Some(&session),
            None,
            json!({
                "operation": "unknown_operation_abcdef01",
                "tool_call_id": "hashed_id_1",
                "error_category": "unknown_operation",
            }),
        )?;
        assert!(
            journal.verify_hash_chain()?,
            "hash chain intact before close"
        );
    }

    {
        let journal = JournalStore::open(&db_path)?;
        let events = journal.events()?;
        assert_eq!(events.len(), 3, "all three events survived reopen");
        // Order + kinds preserved.
        assert_eq!(events[0].kind, JournalEventKind::RunStarted);
        assert_eq!(events[1].kind, JournalEventKind::ToolCallIssued);
        assert_eq!(events[2].kind, JournalEventKind::ToolCallRejected);
        // Payloads preserved.
        assert_eq!(
            events[1].payload.get("operation").and_then(|v| v.as_str()),
            Some("system.status")
        );
        assert_eq!(
            events[2]
                .payload
                .get("error_category")
                .and_then(|v| v.as_str()),
            Some("unknown_operation")
        );
        assert_eq!(events[1].run_id.as_ref(), Some(&run));
        assert_eq!(events[1].session_id.as_ref(), Some(&session));
        // Hash chain intact after reopen.
        assert!(
            journal.verify_hash_chain()?,
            "hash chain intact after reopen"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Compatibility: a fresh DB written with ONLY legacy kinds (no new kinds)
/// still reopens and parses cleanly — the new kinds are not a prerequisite.
#[test]
fn legacy_only_database_still_reopens_cleanly() {
    let dir = std::env::temp_dir().join(format!(
        "m5-legacy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok();
    let db_path = dir.join("legacy.db");

    {
        let journal = JournalStore::open(&db_path).unwrap();
        journal
            .append_event(JournalEventKind::RunStarted, None, None, None, json!({}))
            .unwrap();
        journal
            .append_event(JournalEventKind::RunCompleted, None, None, None, json!({}))
            .unwrap();
        assert!(journal.verify_hash_chain().unwrap());
    }
    {
        let journal = JournalStore::open(&db_path).unwrap();
        let events = journal.events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, JournalEventKind::RunStarted);
        assert_eq!(events[1].kind, JournalEventKind::RunCompleted);
        assert!(
            !events.iter().any(|e| matches!(
                e.kind,
                JournalEventKind::ToolCallIssued | JournalEventKind::ToolCallRejected
            )),
            "no new kinds in a legacy-only DB"
        );
        assert!(journal.verify_hash_chain().unwrap());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

mod common;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::server::{health_snapshot, DispatcherMetrics};
#[test]
fn health_fields_reflect_populated_dispatcher_metrics() -> Result<()> {
    // A metrics handle written to by the loop must surface its state in
    // /health: running flag, last tick timestamp, last error category.
    let config = common::test_config();
    let gateway = Gateway::new(config.clone());
    let journal = JournalStore::in_memory()?;
    let session = common::test_session(&config);
    let run = common::test_run(&config, &session);
    let _approved = common::approved_stdout_invocation(&gateway, &run, &session)?;

    let metrics = DispatcherMetrics::new();
    metrics.record_tick("2026-06-15T12:00:00Z".to_string());
    metrics.record_error_category("timeout".to_string());
    metrics.mark_started();

    let snapshot = health_snapshot(&journal, true, &metrics)?;
    assert_eq!(
        snapshot
            .get("outbox_dispatcher_running")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        snapshot
            .get("last_dispatch_tick_at")
            .and_then(|v| v.as_str()),
        Some("2026-06-15T12:00:00Z")
    );
    assert_eq!(
        snapshot
            .get("last_dispatch_error_category")
            .and_then(|v| v.as_str()),
        Some("timeout")
    );
    Ok(())
}
