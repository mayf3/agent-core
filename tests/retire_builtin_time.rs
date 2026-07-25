//! Retirement integration tests.
#![allow(unused_variables)]
//! Retirement integration tests.
use agent_core_kernel::domain::operation::{is_allowed, lookup, CATALOG};
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::registry::snapshot::{BindingKind, OperationSpec, Risk};
use agent_core_kernel::registry::store::builtin_specs;
use chrono::Utc;
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;
// =========================================================================
// Helper: build a legacy DB with old builtin time.now snapshot
// =========================================================================
fn legacy_specs() -> Vec<OperationSpec> {
    vec![
        OperationSpec {
            name: "time.now".into(),
            risk: Risk::ReadOnly,
            description: "retired builtin".into(),
            parameters: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.time_now".into(),
        },
        OperationSpec {
            name: "stdout.send_text".into(),
            risk: Risk::Write,
            description: "stdout reply".into(),
            parameters: json!({"type":"object"}),
            idempotent: false,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.stdout_send_text".into(),
        },
        OperationSpec {
            name: "session.recall_recent".into(),
            risk: Risk::ReadOnly,
            description: "recall recent".into(),
            parameters: json!({"type":"object"}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.session_recall_recent".into(),
        },
        OperationSpec {
            name: "system.status".into(),
            risk: Risk::ReadOnly,
            description: "system status".into(),
            parameters: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            idempotent: true,
            binding_kind: BindingKind::Builtin,
            binding_key: "builtin.system_status".into(),
        },
        OperationSpec {
            name: "external.example".into(),
            risk: Risk::ReadOnly,
            description: "external example".into(),
            parameters: json!({"type":"object"}),
            idempotent: false,
            binding_kind: BindingKind::External,
            binding_key: "manifest_example".into(),
        },
    ]
}
fn compute_id(specs: &[OperationSpec]) -> String {
    agent_core_kernel::registry::snapshot::compute_snapshot_id(specs).unwrap()
}
/// Create a temp DB with full migrations, then inject an old-format active
/// snapshot containing legacy builtin time.now. Returns (db_path, s1_id).
fn make_legacy_db(label: &str) -> (PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("retire_test_{}_{}", label, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("kernel.sqlite");
    // Step 1: Open via JournalStore to run all migrations.
    let j = JournalStore::open(&db_path).expect("open for migrations");
    drop(j);
    // Step 2: Inject old-format data via raw SQL.
    let conn = Connection::open(&db_path).expect("open raw");
    let s1_specs = legacy_specs();
    let s1_id = compute_id(&s1_specs);
    let ts = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO registry_snapshots (snapshot_id, created_at, operation_count, canonical_digest)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![&s1_id, &ts, s1_specs.len() as i64, &s1_id],
    ).expect("insert S1");
    let mut sorted = s1_specs.clone();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for op in &sorted {
        conn.execute(
            "INSERT OR IGNORE INTO registry_snapshot_operations
             (snapshot_id, operation_name, risk, description, parameters_json, idempotent, binding_kind, binding_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![&s1_id, &op.name, format!("{:?}", op.risk),
                &op.description, serde_json::to_string(&op.parameters).unwrap(),
                op.idempotent as i64, format!("{:?}", op.binding_kind), &op.binding_key],
        ).expect("insert op");
    }
    conn.execute(
        "INSERT OR REPLACE INTO registry_state (singleton_id, active_snapshot_id, version, updated_at)
         VALUES (1, ?1, 1, ?2)",
        rusqlite::params![&s1_id, &ts],
    ).expect("insert registry_state");
    drop(conn);
    (db_path, s1_id)
}
use std::path::PathBuf;
/// Count RegistrySnapshotActivated events with action containing `action`.
fn count_retirement_events(journal: &JournalStore, action: &str) -> usize {
    journal
        .events()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| {
            use agent_core_kernel::domain::JournalEventKind;
            e.kind == JournalEventKind::RegistrySnapshotActivated
                && e.payload
                    .get("action")
                    .and_then(|v| v.as_str())
                    .map(|a| a.contains(action))
                    .unwrap_or(false)
        })
        .count()
}
/// Clean up temp dir.
fn cleanup(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}
// =========================================================================
// §1: Fresh baseline tests
// =========================================================================
#[test]
fn baseline_specs_no_time_now() {
    let specs = builtin_specs();
    assert!(!specs.iter().any(|op| op.name == "time.now"));
    assert_eq!(specs.len(), 6);
}
#[test]
fn catalog_no_time_now() {
    assert!(lookup("time.now").is_none());
    assert!(!is_allowed("time.now"));
    assert!(!CATALOG.iter().any(|s| s.name == "time.now"));
}
// =========================================================================
// §2: Legacy snapshot retired on first restart
// =========================================================================
#[test]
fn legacy_active_snapshot_is_retired_on_restart() {
    let (db_path, s1_id) = make_legacy_db("restart");
    #[allow(unused_variables)]
    let s1_specs = legacy_specs();
    let journal = JournalStore::open(&db_path).expect("open");
    let active_id = journal.initialize_registry().expect("init");
    assert_ne!(active_id, s1_id, "active != S1");
    let s2 = journal.load_registry_snapshot(&active_id).unwrap();
    assert!(
        s2.operations.iter().all(|op| op.name != "time.now"),
        "S2 no time.now"
    );
    let s2n: Vec<&str> = s2.operations.iter().map(|op| op.name.as_str()).collect();
    assert!(s2n.contains(&"session.recall_recent"), "S2 keeps recall");
    assert!(s2n.contains(&"system.status"), "S2 keeps status");
    assert!(s2n.contains(&"stdout.send_text"), "S2 keeps stdout");
    assert!(s2n.contains(&"external.example"), "S2 keeps external");
    let s1 = journal.load_registry_snapshot(&s1_id).unwrap();
    assert!(
        s1.operations.iter().any(|op| op.name == "time.now"),
        "S1 immutable, still has time.now"
    );
    // Event assertions.
    let events = journal.events().unwrap();
    let retire_events: Vec<_> = events
        .iter()
        .filter(|e| {
            use agent_core_kernel::domain::JournalEventKind;
            e.kind == JournalEventKind::RegistrySnapshotActivated
                && e.payload.get("action").and_then(|v| v.as_str()) == Some("retire_builtin_time")
        })
        .collect();
    assert_eq!(retire_events.len(), 1, "exactly 1 retire event");
    let re = &retire_events[0];
    assert_eq!(
        re.payload
            .get("previous_snapshot_id")
            .and_then(|v| v.as_str()),
        Some(s1_id.as_str()),
        "event previous_snapshot_id == S1"
    );
    assert_eq!(
        re.payload.get("new_snapshot_id").and_then(|v| v.as_str()),
        Some(active_id.as_str()),
        "event new_snapshot_id == S2"
    );
    assert!(
        re.payload
            .get("decision_id")
            .and_then(|v| v.as_str())
            .is_some(),
        "decision_id present"
    );
    assert!(
        re.payload
            .get("decision_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("retire_builtin_time:"),
        "decision_id contains retire_builtin_time prefix"
    );
    // Version check: before=1, after=2.
    let conn = Connection::open(&db_path).unwrap();
    let ver: i64 = conn
        .query_row(
            "SELECT version FROM registry_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(
        ver, 2,
        "registry_state.version must be 2 after retirement (was {ver})"
    );
    let db_active: String = conn
        .query_row(
            "SELECT active_snapshot_id FROM registry_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    assert_eq!(
        db_active, active_id,
        "DB active must match returned retired_snapshot_id"
    );
    drop(conn);
    // Legacy snapshot still loadable and unchanged.
    let legacy_before = legacy_specs();
    let legacy_after = journal.load_registry_snapshot(&s1_id).unwrap();
    assert_eq!(
        legacy_after.snapshot_id, s1_id,
        "legacy snapshot_id unchanged"
    );
    assert_eq!(
        legacy_after.operations.len(),
        legacy_before.len(),
        "legacy operation count unchanged"
    );
    // Check time.now still present.
    assert!(
        legacy_after
            .operations
            .iter()
            .any(|op| op.name == "time.now"),
        "legacy snapshot still contains time.now"
    );
    assert!(
        legacy_after
            .operations
            .iter()
            .any(|op| op.name == "external.example"),
        "legacy snapshot still contains external.example"
    );
    cleanup(&db_path);
}
// =========================================================================
// §3: Idempotent across restarts
// =========================================================================
#[test]
fn legacy_time_retirement_is_idempotent_across_restarts() {
    let (db_path, s1_id) = make_legacy_db("idemp");
    let s1_specs = legacy_specs();
    // First boot: version 1 → 2.
    let j1 = JournalStore::open(&db_path).expect("open");
    let a1 = j1.initialize_registry().expect("init1");
    assert_ne!(a1, s1_id, "retired_snapshot_id must differ from legacy");
    let conn1 = Connection::open(&db_path).unwrap();
    let ver1: i64 = conn1
        .query_row(
            "SELECT version FROM registry_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(ver1, 2, "version must be 2 after first boot");
    drop(conn1);
    assert_eq!(
        count_retirement_events(&j1, "retire_builtin_time"),
        1,
        "exactly 1 retirement event after first boot"
    );
    drop(j1);
    // Second boot: version must stay 2, no new snapshot/event.
    let j2 = JournalStore::open(&db_path).expect("open2");
    let a2 = j2.initialize_registry().expect("init2");
    assert_eq!(
        a2, a1,
        "second boot must use same retired_snapshot_id, not create S3"
    );
    let conn2 = Connection::open(&db_path).unwrap();
    let ver2: i64 = conn2
        .query_row(
            "SELECT version FROM registry_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(ver2, 2, "version must remain 2 after second boot (not 3)");
    drop(conn2);
    assert_eq!(
        count_retirement_events(&j2, "retire_builtin_time"),
        1,
        "no extra retirement event on second boot"
    );
    drop(j2);
    cleanup(&db_path);
}
// =========================================================================
// §4: Old snapshot immutable, external ops preserved
// =========================================================================
#[test]
fn legacy_retirement_preserves_operations_and_old_snapshot() {
    let (db_path, s1_id) = make_legacy_db("preserve");
    let s1_specs = legacy_specs();
    let journal = JournalStore::open(&db_path).expect("open");
    // Save legacy snapshot BEFORE retirement (query via raw SQL to avoid
    // double-retirement, then open a fresh JournalStore for the actual test).
    drop(journal);
    let journal2 = JournalStore::open(&db_path).expect("open2");
    let legacy_before = journal2.load_registry_snapshot(&s1_id).unwrap();
    let active_id = journal2.initialize_registry().expect("init");
    // Legacy snapshot: full field-level comparison.
    let legacy_after = journal2.load_registry_snapshot(&s1_id).unwrap();
    assert_eq!(
        legacy_after.snapshot_id, legacy_before.snapshot_id,
        "snapshot_id unchanged"
    );
    assert_eq!(
        legacy_after.operations.len(),
        legacy_before.operations.len(),
        "operation count unchanged"
    );
    for (i, after_op) in legacy_after.operations.iter().enumerate() {
        let before_op = &legacy_before.operations[i];
        assert_eq!(after_op.name, before_op.name, "op[{}].name", i);
        assert_eq!(
            after_op.description, before_op.description,
            "op[{}].desc",
            i
        );
        assert_eq!(
            after_op.parameters, before_op.parameters,
            "op[{}].params",
            i
        );
        assert_eq!(after_op.risk, before_op.risk, "op[{}].risk", i);
        assert_eq!(
            after_op.idempotent, before_op.idempotent,
            "op[{}].idempotent",
            i
        );
        assert_eq!(
            after_op.binding_kind, before_op.binding_kind,
            "op[{}].kind",
            i
        );
        assert_eq!(after_op.binding_key, before_op.binding_key, "op[{}].key", i);
    }
    // Legacy still has time.now.
    assert!(
        legacy_after
            .operations
            .iter()
            .any(|op| op.name == "time.now"),
        "legacy still has time.now"
    );
    assert!(
        legacy_after
            .operations
            .iter()
            .any(|op| op.name == "external.example"),
        "legacy still has external.example"
    );
    // Retired snapshot: no time.now, but preserves external.example.
    let retired = journal2.load_registry_snapshot(&active_id).unwrap();
    assert!(
        retired.operations.iter().all(|op| op.name != "time.now"),
        "retired must NOT have time.now"
    );
    let ext = retired
        .operations
        .iter()
        .find(|op| op.name == "external.example")
        .expect("external.example must be preserved");
    assert_eq!(ext.binding_kind, BindingKind::External);
    assert_eq!(ext.binding_key, "manifest_example");
    // Other builtins preserved.
    for name in &["stdout.send_text", "session.recall_recent", "system.status"] {
        assert!(
            retired.operations.iter().any(|op| op.name == *name),
            "retired must preserve {name}"
        );
    }
    drop(journal2);
    cleanup(&db_path);
}
// =========================================================================
// §5: Stale CAS test (two-Store timing)
//
// The real two-Store stale CAS test is in src/journal/registry_ops.rs
// (unit test with access to pub(crate) apply_builtin_time_retirement).
// This integration test verifies basic CAS conflict handling.
// =========================================================================
#[test]
fn retirement_cas_conflict_does_not_corrupt_active_snapshot() {
    let (db_path, _s1_id) = make_legacy_db("cas");
    let journal = JournalStore::open(&db_path).expect("open");
    // Create a different snapshot S_other.
    let other_spec = OperationSpec {
        name: "system.status".into(),
        risk: Risk::ReadOnly,
        description: "other".into(),
        parameters: json!({"type":"object"}),
        idempotent: true,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.system_status".into(),
    };
    let other = journal.create_registry_snapshot(vec![other_spec]).unwrap();
    let other_id = other.snapshot_id.clone();
    // Activate S_other before retirement: initialize_registry will see
    // this as the active snapshot and skip retirement (no CAS conflict
    // because there's no legacy to detect vs S_other).
    journal.execute_sql_for_test(&format!(
        "UPDATE registry_state SET active_snapshot_id = '{}', version = 2, updated_at = '{}' WHERE singleton_id = 1",
        other_id, Utc::now().to_rfc3339(),
    )).unwrap();
    let active_id = journal.initialize_registry().expect("init");
    assert_ne!(active_id, _s1_id, "must not restore the legacy snapshot");
    let active = journal.load_registry_snapshot(&active_id).unwrap();
    assert!(
        active.lookup("system.status").is_some(),
        "must keep S_other"
    );
    assert!(active.lookup("external.coding_task_submit").is_some());
    assert!(active.lookup("external.coding_hcr_accept").is_some());
    assert_eq!(count_retirement_events(&journal, "retire_builtin_time"), 0);
    drop(journal);
    cleanup(&db_path);
}
// =========================================================================
// §6: Source guards
// =========================================================================
#[test]
fn source_guard_no_builtin_time_dispatch() {
    let execution = include_str!("../src/runtime/tool_execution.rs");
    let dispatch = include_str!("../src/runtime/tool_dispatch.rs");
    // Check production code only (before #[cfg(test)]), ignoring test modules.
    let execution_prod = execution.split("#[cfg(test)]").next().unwrap_or(execution);
    let dispatch_prod = dispatch.split("#[cfg(test)]").next().unwrap_or(dispatch);
    assert_eq!(
        execution_prod.matches("builtin.time_now").count()
            + dispatch_prod.matches("builtin.time_now").count(),
        1,
        "production code must have exactly 1 'builtin.time_now' (retired error path)"
    );
    assert!(dispatch.contains("retired_builtin_operation"));
    assert!(!execution.contains("TimeAdapter"));
    assert!(!dispatch.contains("TimeAdapter"));
}
#[test]
fn source_guard_no_time_adapter() {
    let src = include_str!("../src/adapters/mod.rs");
    assert!(!src.contains("pub struct TimeAdapter"));
    assert!(!src.contains("impl InvocationAdapter for TimeAdapter"));
}
#[test]
fn source_guard_no_time_now_constant() {
    let src = include_str!("../src/domain/operation.rs");
    assert!(!src.contains("pub const TIME_NOW"));
}
// =========================================================================
// §7: Baseline provider tools have no time tools without external harness
// =========================================================================
#[test]
fn baseline_provider_tools_no_time_without_harness() {
    let (db_path, _s1_id) = make_legacy_db("tools");
    let journal = JournalStore::open(&db_path).expect("open");
    let active_id = journal.initialize_registry().expect("init");
    let grants = vec![
        "stdout.send_text".into(),
        "session.recall_recent".into(),
        "system.status".into(),
    ];
    let snap = journal.load_registry_snapshot(&active_id).unwrap();
    let provider_tools = snap.provider_tools_for_grants(&grants);
    let tools: Vec<&str> = provider_tools
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(|v| v.as_str()))
        .collect();
    assert!(
        !tools.contains(&"time.now"),
        "no time.now in tools: {tools:?}"
    );
    assert!(
        !tools.contains(&"external.time_now"),
        "no ext time in tools: {tools:?}"
    );
    drop(journal);
    cleanup(&db_path);
}
