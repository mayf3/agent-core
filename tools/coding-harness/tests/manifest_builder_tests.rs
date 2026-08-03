//! Manifest builder determinism and uniqueness tests.
//!
//! Extracted from `schema_tests.rs` to keep module sizes under the 500-line
//! structure limit.

use coding_harness::operation_specs;

#[test]
fn coding_manifests_are_deterministic_and_unique() {
    let ep = "http://127.0.0.1:7200";
    let ad = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let ws = vec!["agent-dev".to_string()];
    let m1 = operation_specs::build_manifests(&ws, ep, ad);
    let m2 = operation_specs::build_manifests(&ws, ep, ad);
    assert_eq!(m1.len(), 7);
    for i in 0..7 {
        assert_eq!(
            m1[i].manifest_id, m2[i].manifest_id,
            "deterministic: {}",
            m1[i].operation_name
        );
        assert_eq!(
            m1[i].compute_manifest_id().unwrap(),
            m1[i].manifest_id,
            "compute matches: {}",
            m1[i].operation_name
        );
    }
    for m in &m1 {
        let s = serde_json::to_string(&m.input_schema).unwrap();
        assert!(
            !s.contains("/tmp/"),
            "no workspace root: {}",
            m.operation_name
        );
        assert!(!s.contains("sk-"), "no secret: {}", m.operation_name);
    }
}
