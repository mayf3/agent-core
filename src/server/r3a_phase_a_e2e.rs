//! PR3A security tests that do not simulate Harness success.
//!
//! The successful five-gate path lives in Coding Harness's Linux-only real
//! E2E.  These Kernel tests cover routing and fail-closed Proposal derivation.

use crate::journal::JournalStore;
use crate::server::coding_router::parse_coding_intent;

#[test]
fn north_star_routes_to_structured_intent() {
    let intent = parse_coding_intent("开发一个 external.calculator，支持加减乘除").unwrap();
    assert_eq!(intent.operation, "external.calculator");
    assert_eq!(intent.functions, ["add", "subtract", "multiply", "divide"]);
    assert_eq!(intent.schema_version, "calculator-fixture-v0");
    assert_eq!(
        intent.development_request.build_profile,
        "invocable-capability-v0"
    );
}

#[test]
fn unsupported_capability_does_not_route() {
    assert!(parse_coding_intent("开发一个浏览器").is_err());
    assert!(parse_coding_intent("create a web server").is_err());
}

#[test]
fn baseline_contains_active_coding_control_but_not_retired_hcr_or_calculator() {
    let journal = JournalStore::in_memory().unwrap();
    let snapshot = journal
        .load_registry_snapshot(&journal.current_registry_snapshot_id().unwrap())
        .unwrap();
    assert!(snapshot.lookup("external.coding_task_submit").is_some());
    assert!(snapshot.lookup("external.coding_hcr_accept").is_none());
    assert!(snapshot.lookup("external.calculator").is_none());
}
