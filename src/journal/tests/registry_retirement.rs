use crate::domain::JournalEventKind;
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use crate::registry::store::builtin_specs;

fn legacy_s1_specs() -> Vec<OperationSpec> {
    let mut specs = builtin_specs();
    specs.push(OperationSpec {
        name: "time.now".into(),
        risk: Risk::ReadOnly,
        description: "retired".into(),
        parameters: serde_json::json!({"type":"object"}),
        idempotent: true,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.time_now".into(),
    });
    specs
}

/// Real stale CAS test with two JournalStore instances.
///
/// Timeline:
/// 1. DB active = legacy_snapshot_id (S1 with builtin time.now)
/// 2. Store A opens, reads S1, creates S_retired (S1 minus time.now)
/// 3. Store A caches S1 (simulating stale cache before CAS)
/// 4. Store B opens, creates S_newer (unrelated snapshot)
/// 5. Store B activates S_newer → DB active = S_newer
/// 6. Store A calls apply_builtin_time_retirement(expected=S1, new=S_retired)
/// 7. CAS fails (DB has S_newer, not S1)
/// 8. Store A cache refreshed to S_newer
#[test]
fn stale_retirement_cas_refreshes_cache() {
    let dir = std::env::temp_dir().join(format!("registry_cache_stale_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("kernel.sqlite");

    // 1. Create DB with legacy S1.
    let j_init = JournalStore::open(&db_path).expect("open init");
    let _ = j_init.initialize_registry().expect("init");
    // Inject legacy time.now into the active snapshot.
    // We need to create a NEW snapshot that includes time.now.
    let s1_specs = legacy_s1_specs();
    let s1_snap = j_init
        .create_registry_snapshot(s1_specs)
        .expect("create S1");
    let s1_id = s1_snap.snapshot_id.clone();
    // Overwrite registry_state to point to S1.
    j_init.execute_sql_for_test(&format!(
            "UPDATE registry_state SET active_snapshot_id = '{}', version = 1, updated_at = '{}' WHERE singleton_id = 1",
            s1_id, chrono::Utc::now().to_rfc3339(),
        )).expect("set active to S1");
    // Re-cache (since initialize_registry set cache to baseline).
    j_init.set_current_snapshot_id_for_test(&s1_id);
    drop(j_init);

    // 2-3. Store A: open, cache S1, create S_retired snapshot.
    let store_a = JournalStore::open(&db_path).expect("store_a open");
    store_a.set_current_snapshot_id_for_test(&s1_id);

    let s1 = store_a.load_registry_snapshot(&s1_id).expect("load S1");
    let retired_specs: Vec<OperationSpec> = s1
        .operations
        .iter()
        .filter(|op| {
            !(op.name == "time.now"
                && op.binding_kind == BindingKind::Builtin
                && op.binding_key == "builtin.time_now")
        })
        .cloned()
        .collect();
    let retired = store_a
        .create_registry_snapshot(retired_specs)
        .expect("create S_retired");
    let retired_id = retired.snapshot_id.clone();

    // 4-5. Store B: create and activate S_newer.
    let store_b = JournalStore::open(&db_path).expect("store_b open");
    let newer_spec = OperationSpec {
        name: "system.status".into(),
        risk: Risk::ReadOnly,
        description: "newer".into(),
        parameters: serde_json::json!({"type":"object"}),
        idempotent: true,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.system_status".into(),
    };
    let newer = store_b
        .create_registry_snapshot(vec![newer_spec])
        .expect("create S_newer");
    let newer_id = newer.snapshot_id.clone();
    store_b.execute_sql_for_test(&format!(
            "UPDATE registry_state SET active_snapshot_id = '{}', version = 2, updated_at = '{}' WHERE singleton_id = 1",
            newer_id, chrono::Utc::now().to_rfc3339(),
        )).expect("store_b activate");
    store_b
        .activate_registry_snapshot(&newer_id)
        .expect("store_b cache");
    let b_cache = store_b.get_current_snapshot_id_for_test();
    assert_eq!(
        b_cache,
        Some(newer_id.clone()),
        "Store B cache must be newer"
    );
    drop(store_b);

    // 6-7. Store A: apply with stale expected=S1 → CAS must fail.
    let decision_id = format!("retire_builtin_time_stale:{}", s1_id);
    let result = store_a.apply_builtin_time_retirement(&retired_id, &s1_id, &decision_id);
    assert!(
        result.is_err(),
        "CAS must fail with stale expected snapshot"
    );
    let err = format!("{}", result.as_ref().unwrap_err());
    assert!(
        err.contains("snapshot_conflict"),
        "error must be snapshot_conflict: {err}"
    );

    // 8. Store A cache refreshed.
    let a_cache = store_a.get_current_snapshot_id_for_test();
    assert_eq!(
        a_cache,
        Some(newer_id.clone()),
        "Store A cache must be refreshed to newer snapshot"
    );

    // DB active == newer.
    let db_active = store_a
        .load_active_snapshot_from_state()
        .expect("db active");
    assert_eq!(db_active, Some(newer_id.clone()), "DB active must be newer");

    // Snapshots still exist.
    let _ = store_a.load_registry_snapshot(&s1_id).expect("S1 exists");
    let _ = store_a
        .load_registry_snapshot(&retired_id)
        .expect("S_retired exists");
    let _ = store_a
        .load_registry_snapshot(&newer_id)
        .expect("S_newer exists");

    // No retirement event written.
    let events = store_a.events().expect("events");
    let retire_count = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::RegistrySnapshotActivated
                && e.payload
                    .get("action")
                    .and_then(|v: &serde_json::Value| v.as_str())
                    == Some("retire_builtin_time")
        })
        .count();
    assert_eq!(retire_count, 0, "no retirement event after failed CAS");

    // No retire_builtin_time activation was written.
    // (Store B's activation was via raw SQL which doesn't write journal events.)

    drop(store_a);
    let _ = std::fs::remove_dir_all(&dir);
}
