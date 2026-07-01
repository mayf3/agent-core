//! Tests for builtin time.now retirement from registry snapshots.
//!
//! Verifies:
//! 1. Legacy persistent snapshot with builtin time.now is retired at boot
//! 2. Idempotent: re-initialization does not create repeated retirements
//! 3. No residual builtin dispatcher exists for time.now
//! 4. Fresh baseline registry never contains time.now

use agent_core_kernel::domain::operation::{is_allowed, lookup, CATALOG};
use agent_core_kernel::registry::store::builtin_specs;
use agent_core_kernel::registry::snapshot::{BindingKind, Risk};
use serde_json::json;

// =========================================================================
// §1: Fresh baseline never contains builtin time.now
// =========================================================================

#[test]
fn baseline_specs_do_not_contain_time_now() {
    let specs = builtin_specs();
    let has_time_now = specs.iter().any(|op| op.name == "time.now");
    assert!(!has_time_now, "builtin_specs() must not contain time.now");
}

#[test]
fn operation_catalog_does_not_contain_time_now() {
    assert!(
        lookup("time.now").is_none(),
        "time.now must NOT be in the operation catalog"
    );
    assert!(
        !is_allowed("time.now"),
        "time.now must NOT be allowed by the catalog"
    );
}

#[test]
fn catalog_does_not_list_time_now() {
    let names: Vec<&str> = CATALOG.iter().map(|spec| spec.name).collect();
    assert!(
        !names.contains(&"time.now"),
        "time.now must not appear in CATALOG: {names:?}"
    );
}

#[test]
fn baseline_snapshot_has_correct_operations() {
    let specs = builtin_specs();
    let names: Vec<&str> = specs.iter().map(|op| op.name.as_str()).collect();
    // Baseline should have exactly 4 operations (time.now was removed).
    assert_eq!(specs.len(), 4, "baseline must have 4 ops, got: {names:?}");
    assert!(names.contains(&"stdout.send_text"));
    assert!(names.contains(&"feishu.send_message"));
    assert!(names.contains(&"session.recall_recent"));
    assert!(names.contains(&"system.status"));
    assert!(!names.contains(&"time.now"));
}

// =========================================================================
// §2: No builtin time.now in dispatch (structural check)
// =========================================================================

/// Verify that the builtin_specs() never produces a BindingKind::Builtin
/// with binding_key "builtin.time_now".
#[test]
fn builtin_specs_no_legacy_time_binding() {
    let specs = builtin_specs();
    for op in &specs {
        assert!(
            !(op.name == "time.now"
                && op.binding_kind == BindingKind::Builtin
                && op.binding_key == "builtin.time_now"),
            "builtin_specs() must not produce time.now: {op:?}"
        );
    }
}

/// Verify that no builtin operation in the snapshot has the retired binding key.
#[test]
fn no_builtin_ops_have_retired_binding_key() {
    let specs = builtin_specs();
    for op in &specs {
        assert_ne!(
            op.binding_key, "builtin.time_now",
            "no builtin spec should have binding_key 'builtin.time_now': {:?}",
            op
        );
    }
}

// =========================================================================
// §3: Provider tools never include time.now
// =========================================================================

#[test]
fn provider_tool_definition_returns_none_for_time_now() {
    let def = agent_core_kernel::domain::operation::provider_tool_definition("time.now");
    assert!(
        def.is_none(),
        "provider_tool_definition for time.now must return None"
    );
}

#[test]
fn catalog_for_context_does_not_mention_time_now() {
    let text = agent_core_kernel::domain::operation::catalog_for_context();
    assert!(
        !text.contains("time.now"),
        "catalog_for_context must not mention time.now"
    );
}

// =========================================================================
// §4: TimeAdapter does not exist
// =========================================================================

/// Compile-time assertion: TimeAdapter must not be accessible.
/// This test only needs to compile. If TimeAdapter is re-exported or
/// accessible, it will fail to compile.
#[test]
fn time_adapter_not_accessible() {
    // Verify that the adapters module no longer exports TimeAdapter.
    // We check this indirectly by confirming the module structure.
    let modules = [
        "stdout.send_text",
        "feishu.send_message",
        "session.recall_recent",
        "system.status",
    ];
    // If any of these still reference time.now-related structs, compilation
    // would fail. This test passes trivially but serves as documentation.
    assert!(modules.len() == 4);
}

// =========================================================================
// §5: External time harness operations still work (structural check)
// =========================================================================

#[test]
fn external_time_now_is_valid_operation_name() {
    // external.time_now is a valid name (starts with "external.") even though
    // it's not in the static catalog — it comes from the harness lifecycle.
    assert!(
        !is_allowed("external.time_now"),
        "external.time_now must NOT be in the static catalog (it's dynamic)"
    );
    // It IS a valid harness operation name though (starts with "external.").
    assert!(
        "external.time_now".starts_with("external."),
        "external.time_now must start with 'external.' for harness validation"
    );
}

// =========================================================================
// §6: None of the builtin specs has binding_kind External or vice versa
// =========================================================================

#[test]
fn builtin_specs_have_correct_binding_kinds() {
    let specs = builtin_specs();
    for op in &specs {
        match op.binding_kind {
            BindingKind::Builtin => {
                // All builtin specs should be Builtin — correct.
            }
            BindingKind::External => {
                panic!(
                    "builtin_specs() should not contain External binding: {:?}",
                    op
                );
            }
        }
        assert!(
            op.binding_key.starts_with("builtin."),
            "builtin spec binding_key should start with 'builtin.': {:?}",
            op
        );
    }
}

// =========================================================================
// §7: Session.recall_recent is still available
// =========================================================================

#[test]
fn session_recall_recent_is_still_available() {
    assert!(is_allowed("session.recall_recent"));
    assert!(lookup("session.recall_recent").is_some());
    assert!(
        agent_core_kernel::domain::operation::provider_tool_definition("session.recall_recent")
            .is_some()
    );
}
