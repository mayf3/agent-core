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

// =========================================================================
// §1: Real stale CAS — Store B uses real transactional activation
// =========================================================================

#[test]
fn stale_retirement_cas_refreshes_cache() {
    let dir = std::env::temp_dir().join(format!("registry_cache_stale_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("kernel.sqlite");

    // 1. DB active = legacy_snapshot_id (S1 with builtin time.now)
    let j_init = JournalStore::open(&db_path).expect("open init");
    let _ = j_init.initialize_registry().expect("init");
    let s1_specs = legacy_s1_specs();
    let s1_snap = j_init
        .create_registry_snapshot(s1_specs)
        .expect("create S1");
    let s1_id = s1_snap.snapshot_id.clone();
    // Set active to S1 via SQL (initial setup, not a concurrent activation).
    j_init.execute_sql_for_test(&format!(
        "UPDATE registry_state SET active_snapshot_id = '{}', version = 1, updated_at = '{}' WHERE singleton_id = 1",
        s1_id, chrono::Utc::now().to_rfc3339(),
    )).expect("set active to S1");
    j_init.set_current_snapshot_id_for_test(&s1_id);
    drop(j_init);

    // 2. Store A: open, cache S1, create S_retired snapshot.
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

    // 3. Store B: open, create S_newer, activate via REAL production path.
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

    // Store B activates via the production transactional function.
    let b_result = store_b
        .activate_snapshot_transactional(
            &s1_id,
            &newer_id,
            "store_b_activation",
            "manual_activation",
        )
        .expect("Store B activation must succeed");
    assert_eq!(b_result.active_snapshot_id, newer_id);
    assert_eq!(b_result.previous_snapshot_id, s1_id);

    // 4. Verify Store B's cache.
    let b_cache = store_b.get_current_snapshot_id_for_test();
    assert_eq!(
        b_cache,
        Some(newer_id.clone()),
        "Store B cache must be newer"
    );
    drop(store_b);

    // 5. Store A: apply with stale expected=S1 → CAS must fail.
    let decision_id = format!("retire_builtin_time_stale:{}", s1_id);
    let result = store_a.apply_builtin_time_retirement(&retired_id, &s1_id, &decision_id);
    assert!(
        result.is_err(),
        "CAS must fail with stale expected snapshot"
    );
    assert!(
        format!("{}", result.as_ref().unwrap_err()).contains("snapshot_conflict"),
        "error must be snapshot_conflict"
    );

    // 6. Store A cache refreshed to newer.
    let a_cache = store_a.get_current_snapshot_id_for_test();
    assert_eq!(
        a_cache,
        Some(newer_id.clone()),
        "Store A cache must be refreshed to newer"
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

    // Event counts.
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

    let b_activation_count = events
        .iter()
        .filter(|e| {
            e.kind == JournalEventKind::RegistrySnapshotActivated
                && e.payload
                    .get("action")
                    .and_then(|v: &serde_json::Value| v.as_str())
                    == Some("manual_activation")
        })
        .count();
    assert_eq!(b_activation_count, 1, "Store B activation event exists");

    // 7. Fresh Store C initialization must not return legacy.
    drop(store_a);
    let store_c = JournalStore::open(&db_path).expect("store_c open");
    let active = store_c.initialize_registry().expect("init store_c");
    assert_ne!(active, s1_id, "fresh init must not return legacy");
    let snap = store_c.load_registry_snapshot(&active).unwrap();
    assert!(
        snap.operations.iter().all(|op| op.name != "time.now"),
        "no time.now after re-init"
    );
    assert!(
        snap.lookup("external.coding_task_submit").is_some()
            && snap.lookup("external.coding_hcr_accept").is_some(),
        "restart upgrade must preserve/seed the controlled coding operations"
    );

    drop(store_c);
    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================================
// §2: CAS refresh failure clears legacy cache
// =========================================================================

#[test]
fn retirement_cas_refresh_failure_clears_legacy_cache() {
    let dir = std::env::temp_dir().join(format!("registry_cache_fail_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("kernel.sqlite");

    // Setup: legacy S1 active.
    let j_init = JournalStore::open(&db_path).expect("open init");
    let _ = j_init.initialize_registry().expect("init");
    let s1_specs = legacy_s1_specs();
    let s1_snap = j_init
        .create_registry_snapshot(s1_specs)
        .expect("create S1");
    let s1_id = s1_snap.snapshot_id.clone();
    j_init.execute_sql_for_test(&format!(
        "UPDATE registry_state SET active_snapshot_id = '{}', version = 1, updated_at = '{}' WHERE singleton_id = 1",
        s1_id, chrono::Utc::now().to_rfc3339(),
    )).expect("set active");
    j_init.set_current_snapshot_id_for_test(&s1_id);
    drop(j_init);

    // Store A: open, cache legacy, create retired snapshot.
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

    // Store B: activate different snapshot via real transactional path.
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
    store_b
        .activate_snapshot_transactional(
            &s1_id,
            &newer_id,
            "store_b_activation",
            "manual_activation",
        )
        .expect("B activation");
    drop(store_b);

    // Verify Store A still has legacy in cache.
    assert_eq!(
        store_a.get_current_snapshot_id_for_test(),
        Some(s1_id.clone()),
        "Store A should still have legacy cached initially"
    );

    // Destroy registry_state to make refresh_cache_from_db fail.
    store_a
        .execute_sql_for_test("DROP TABLE registry_state")
        .expect("drop registry_state");

    // Now call apply_builtin_time_retirement — CAS will fail, refresh will fail.
    let result = store_a.apply_builtin_time_retirement(&retired_id, &s1_id, "fail_test");
    assert!(result.is_err(), "must fail");

    // After CAS failure + refresh failure, cache must be None (not legacy).
    let cache = store_a.get_current_snapshot_id_for_test();
    assert_eq!(
        cache, None,
        "cache must be None after failed refresh, not legacy: {cache:?}"
    );

    // initialize_registry on a fresh store with no registry_state must fail.
    drop(store_a);
    let store_c = JournalStore::open(&db_path).expect("store_c open");
    let result_c = store_c.initialize_registry();
    assert!(
        result_c.is_err(),
        "initialize_registry must fail with missing registry_state"
    );

    drop(store_c);
    let _ = std::fs::remove_dir_all(&dir);
}
