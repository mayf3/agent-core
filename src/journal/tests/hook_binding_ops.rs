//! Online HookBinding bind / unbind — journal-layer security semantics.

use crate::domain::{
    ApprovedHookBindingChange, HookBindingChangeAction, HookBindingChangeIntent,
    JournalEventKind,
};
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, HookBinding};

fn context_binding(endpoint: &str, provider: &str) -> HookBinding {
    HookBinding {
        contract: "context.prepare.v0".into(),
        hook_id: "external:context-harness-v0".into(),
        hook_version: "v0".into(),
        binding_kind: BindingKind::External,
        binding_key: "external.context_harness".into(),
        provider_id: provider.into(),
        endpoint: endpoint.into(),
    }
}

fn bind_intent(expected: &str, binding: HookBinding) -> HookBindingChangeIntent {
    HookBindingChangeIntent {
        action: HookBindingChangeAction::Bind,
        contract: binding.contract.clone(),
        binding: Some(binding),
        expected_snapshot_id: expected.into(),
        requested_by: "ipc_operator".into(),
    }
}

fn unbind_intent(expected: &str, contract: &str) -> HookBindingChangeIntent {
    HookBindingChangeIntent {
        action: HookBindingChangeAction::Unbind,
        contract: contract.into(),
        binding: None,
        expected_snapshot_id: expected.into(),
        requested_by: "ipc_operator".into(),
    }
}

fn approved(intent: HookBindingChangeIntent) -> ApprovedHookBindingChange {
    ApprovedHookBindingChange {
        intent,
        decision_id: format!(
            "decision_{}",
            uuid::Uuid::new_v4().simple()
        ),
    }
}

fn current_id(j: &JournalStore) -> String {
    j.current_registry_snapshot_id().unwrap()
}

#[test]
fn bind_hook_expected_correct_activates_new_snapshot() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);

    let result = j
        .bind_hook(&approved(bind_intent(&s0, context_binding(
            "http://127.0.0.1:7401/context",
            "agent-context-harness-v0",
        ))))
        .unwrap();
    assert!(result.changed);
    assert_eq!(result.previous_snapshot_id, s0);
    let s1 = result.active_snapshot_id;
    assert_ne!(s1, s0);
    assert_eq!(current_id(&j), s1);

    let snap = j.load_registry_snapshot(&s1).unwrap();
    assert_eq!(
        snap.hook_binding("context.prepare.v0").unwrap().endpoint,
        "http://127.0.0.1:7401/context"
    );
}

#[test]
fn bind_hook_stale_expected_snapshot_conflicts() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);

    let err = j
        .bind_hook(&approved(bind_intent(
            "snap_stale_deadbeef",
            context_binding("http://127.0.0.1:7401/context", "p"),
        )))
        .unwrap_err();
    assert!(err.to_string().contains("snapshot_conflict"), "got: {err}");
    // Active snapshot unchanged.
    assert_eq!(current_id(&j), s0);
}

#[test]
fn bind_preserves_other_operations_and_hook_bindings() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);
    let s0_snap = j.load_registry_snapshot(&s0).unwrap();
    let ops_before: Vec<String> = s0_snap
        .operations
        .iter()
        .map(|op| op.name.clone())
        .collect();
    let bindings_before: Vec<String> = s0_snap
        .hook_bindings
        .iter()
        .map(|b| b.contract.clone())
        .collect();

    let result = j
        .bind_hook(&approved(bind_intent(&s0, context_binding("http://a", "p"))))
        .unwrap();
    let s1 = j.load_registry_snapshot(&result.active_snapshot_id).unwrap();

    // Operations verbatim.
    let ops_after: Vec<String> = s1.operations.iter().map(|op| op.name.clone()).collect();
    assert_eq!(ops_after, ops_before);
    // Pre-existing hook bindings preserved; exactly one context binding added.
    let bindings_after: Vec<String> = s1
        .hook_bindings
        .iter()
        .map(|b| b.contract.clone())
        .collect();
    for contract in &bindings_before {
        assert!(bindings_after.contains(contract), "missing {contract}");
    }
    assert_eq!(
        s1.hook_bindings
            .iter()
            .filter(|b| b.contract == "context.prepare.v0")
            .count(),
        1
    );
}

#[test]
fn bind_replace_semantics_and_idempotent_noop() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);

    // Bind A.
    let r1 = j
        .bind_hook(&approved(bind_intent(
            &s0,
            context_binding("http://127.0.0.1:7401", "p-a"),
        )))
        .unwrap();
    // Bind B on the same contract: replace, still exactly one binding.
    let r2 = j
        .bind_hook(&approved(bind_intent(
            &r1.active_snapshot_id,
            context_binding("http://127.0.0.1:7402", "p-b"),
        )))
        .unwrap();
    assert!(r2.changed);
    let s2 = j.load_registry_snapshot(&r2.active_snapshot_id).unwrap();
    let binding = s2.hook_binding("context.prepare.v0").unwrap();
    assert_eq!(binding.endpoint, "http://127.0.0.1:7402");
    assert_eq!(
        s2.hook_bindings
            .iter()
            .filter(|b| b.contract == "context.prepare.v0")
            .count(),
        1
    );

    // Re-binding the identical binding is an idempotent no-op.
    let r3 = j
        .bind_hook(&approved(bind_intent(
            &r2.active_snapshot_id,
            context_binding("http://127.0.0.1:7402", "p-b"),
        )))
        .unwrap();
    assert!(!r3.changed);
    assert_eq!(r3.active_snapshot_id, r2.active_snapshot_id);
}

#[test]
fn unbind_removes_only_target_contract() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);
    let r1 = j
        .bind_hook(&approved(bind_intent(&s0, context_binding("http://a", "p"))))
        .unwrap();

    let r2 = j
        .unbind_hook(&approved(unbind_intent(
            &r1.active_snapshot_id,
            "context.prepare.v0",
        )))
        .unwrap();
    assert!(r2.changed);
    let s2 = j.load_registry_snapshot(&r2.active_snapshot_id).unwrap();
    assert!(s2.hook_binding("context.prepare.v0").is_none());
    // The budget binding is untouched.
    assert!(s2.hook_binding("run.budget.resolve.v0").is_some());
    // Operations untouched.
    assert_eq!(s2.operations.len(), j.load_registry_snapshot(&s0).unwrap().operations.len());

    // Unbinding a contract that is not bound: idempotent no-op.
    let r3 = j
        .unbind_hook(&approved(unbind_intent(
            &r2.active_snapshot_id,
            "context.prepare.v0",
        )))
        .unwrap();
    assert!(!r3.changed);
}

#[test]
fn bind_external_missing_provider_id_rejected() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);
    let mut binding = context_binding("http://a", "p");
    binding.provider_id = String::new();
    let err = j
        .bind_hook(&approved(bind_intent(&s0, binding)))
        .unwrap_err();
    assert!(
        err.to_string().contains("provider_id"),
        "got: {err}"
    );
    assert_eq!(current_id(&j), s0);
}

#[test]
fn bind_external_missing_endpoint_rejected() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);
    let mut binding = context_binding("http://a", "p");
    binding.endpoint = String::new();
    let err = j
        .bind_hook(&approved(bind_intent(&s0, binding)))
        .unwrap_err();
    assert!(err.to_string().contains("endpoint"), "got: {err}");
    assert_eq!(current_id(&j), s0);
}

#[test]
fn bind_writes_registry_snapshot_activated_audit_event() {
    let j = JournalStore::in_memory().unwrap();
    let s0 = current_id(&j);
    let appr = approved(bind_intent(&s0, context_binding("http://a", "p")));
    let decision_id = appr.decision_id.clone();
    j.bind_hook(&appr).unwrap();

    let events = j.events().unwrap();
    let activated: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::RegistrySnapshotActivated)
        .collect();
    assert!(!activated.is_empty());
    let last = activated.last().unwrap();
    assert_eq!(last.payload.get("action").and_then(|v| v.as_str()), Some("hook_binding_bind"));
    assert_eq!(last.payload.get("decision_id").and_then(|v| v.as_str()), Some(decision_id.as_str()));
    assert!(last.payload.get("new_snapshot_id").is_some());
    assert!(last.payload.get("previous_snapshot_id").is_some());
}

#[test]
fn active_snapshot_persisted_and_restored_after_reopen() {
    let dir = std::env::temp_dir().join(format!("hook_binding_reopen_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("kernel.sqlite");

    // First process: initialize + bind.
    let j1 = JournalStore::open(&db_path).unwrap();
    j1.initialize_registry().unwrap();
    let s0 = current_id(&j1);
    let r1 = j1
        .bind_hook(&approved(bind_intent(&s0, context_binding("http://a", "p"))))
        .unwrap();

    drop(j1);

    // Restart: the new store must restore the LATEST active snapshot.
    let j2 = JournalStore::open(&db_path).unwrap();
    let restored = j2.initialize_registry().unwrap();
    assert_eq!(restored, r1.active_snapshot_id, "restart must restore latest active");
    let snap = j2.load_registry_snapshot(&restored).unwrap();
    assert!(snap.hook_binding("context.prepare.v0").is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Content-addressed reuse at the CREATE layer: creating a snapshot with
/// IDENTICAL operations + hook bindings reuses the canonical snapshot
/// (same id, same created_at) instead of generating a duplicate. Verified
/// with a `context.prepare.v0` binding to prove the mechanism is
/// contract-generic. This is the底层 property that makes `bind_hook`'s
/// identical-rebind no-op and hot-update rollback cheap.
#[test]
fn create_snapshot_with_identical_bindings_reuses_existing_snapshot() {
    let journal = JournalStore::in_memory().unwrap();
    let specs = crate::registry::store::builtin_specs();
    let bindings = vec![
        HookBinding::builtin_budget(),
        HookBinding {
            contract: "context.prepare.v0".to_string(),
            hook_id: "external:context-provider-alpha".to_string(),
            hook_version: "v0".to_string(),
            binding_kind: BindingKind::External,
            binding_key: "external.context_provider_alpha".to_string(),
            provider_id: "context-provider-alpha".to_string(),
            endpoint: "http://127.0.0.1:17400/context.prepare.v0".to_string(),
        },
    ];

    let first = journal
        .create_registry_snapshot_with_hook_bindings(specs.clone(), bindings.clone())
        .unwrap();
    let second = journal
        .create_registry_snapshot_with_hook_bindings(specs.clone(), bindings.clone())
        .unwrap();

    assert_eq!(
        second.snapshot_id, first.snapshot_id,
        "identical content must reuse the canonical snapshot, not create a duplicate"
    );
    assert_eq!(
        second.created_at, first.created_at,
        "the existing snapshot row is reused, not recreated"
    );
}
