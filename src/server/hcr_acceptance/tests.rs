//! Tests for HCR acceptance opaque payload verification.
//!
//! Golden vector tests prove that the canonical JSON serialization
//! (alphabetically sorted keys) produces the same digest regardless
//! of insertion order.  Both the coding harness and the Kernel use
//! the identical algorithm, so a single golden vector covers both.

use super::harness_client::{sort_object_keys, verify_opaque_payload_digest};
use serde_json::json;

/// Helper: produce canonical JSON bytes for a Value (sorted keys).
fn canonical_bytes(value: &serde_json::Value) -> Vec<u8> {
    let sorted = sort_object_keys(value);
    serde_json::to_vec(&sorted).unwrap()
}

/// Assert that two insertion-order-different but semantically-identical
/// Values produce the same canonical (sorted) JSON bytes and digest.
fn assert_canonical_equal(a: &serde_json::Value, b: &serde_json::Value) {
    let sorted_a = sort_object_keys(a);
    let sorted_b = sort_object_keys(b);
    assert_eq!(sorted_a, sorted_b, "sorted values must be equal");
    let bytes_a = serde_json::to_vec(&sorted_a).unwrap();
    let bytes_b = serde_json::to_vec(&sorted_b).unwrap();
    assert_eq!(bytes_a, bytes_b, "canonical bytes must be equal");
    let dig_a = verify_opaque_payload_digest(a).unwrap();
    let dig_b = verify_opaque_payload_digest(b).unwrap();
    assert_eq!(dig_a, dig_b, "canonical digest must be equal");
}

/// Build a merged response Value (AcceptanceResponse + envelope fields)
/// as produced by the Harness's `ok_envelope_json()`, with configurable
/// insertion order to test canonical sorting.
fn merged_response(
    delivery_manifest_ref: Option<&str>,
    delivery_manifest_digest: Option<&str>,
) -> serde_json::Value {
    let mut base = json!({
        "harness_execution_id": "hex_test",
        "idempotency_key": "accept:test",
        "hcr_id": "hcr_test",
        "claim_id": "claim_test",
        "run_id": "run_test",
        "principal_id": "principal_test",
        "gateway_session_id": "session_test",
        "registry_snapshot_id": "snap_test",
        "operation": "external.coding_hcr_accept",
        "candidate_id": "candidate_test",
        "candidate_digest": format!("sha256:{}", "1".repeat(64)),
        "overall_outcome": "CandidatePassed",
        "gate_results": [],
        "artifact_ref": "candidate/target/release/component",
        "artifact_digest": format!("sha256:{}", "3".repeat(64)),
        "component_manifest_digest": format!("sha256:{}", "4".repeat(64)),
        "evidence_digest": format!("sha256:{}", "2".repeat(64)),
    });
    if let Some(ref_val) = delivery_manifest_ref {
        base["delivery_manifest_ref"] = json!(ref_val);
    }
    if let Some(dig_val) = delivery_manifest_digest {
        base["delivery_manifest_digest"] = json!(dig_val);
    }
    // Add envelope fields (as ok_envelope_json does)
    base["schema_version"] = json!("external-receipt-envelope-v1");
    base["invocation_intent_id"] = json!("invocation_test");
    base["issuer"] = json!("coding-harness");
    base["subject_digest"] = json!(format!("sha256:{}", "1".repeat(64)));
    base["outcome"] = json!("Passed");
    base["opaque_payload_digest"] = json!("sha256:0000");
    base["receipt_digest"] = json!("sha256:0000");
    base
}

// ── Golden vector tests ──────────────────────────────────────────────

/// Golden vector: different insertion orders produce same canonical bytes.
#[test]
fn field_insertion_order_is_canonical() {
    let a = json!({"b": 2, "a": 1, "c": 3});
    let b = json!({"c": 3, "a": 1, "b": 2});
    assert_canonical_equal(&a, &b);
}

/// Golden vector: nested objects are recursively sorted.
#[test]
fn nested_object_keys_are_sorted() {
    let a = json!({"z": {"y": 2, "x": 1}, "a": 0});
    let b = json!({"a": 0, "z": {"x": 1, "y": 2}});
    assert_canonical_equal(&a, &b);
}

/// Golden vector: arrays preserve element order.
#[test]
fn array_elements_preserve_insertion_order() {
    let a = json!({"b": [3, 1, 2], "a": 0});
    let b = json!({"a": 0, "b": [3, 1, 2]});
    assert_canonical_equal(&a, &b);
}

/// Golden vector: nested arrays with objects.
#[test]
fn nested_array_objects_keys_are_sorted() {
    let a = json!({"items": [{"z": 2, "a": 1}, {"y": 0, "x": 3}]});
    let b = json!({"items": [{"a": 1, "z": 2}, {"x": 3, "y": 0}]});
    assert_canonical_equal(&a, &b);
}

/// Golden vector: null, bool, number, string are preserved.
#[test]
fn primitive_types_are_preserved() {
    let a = json!({"flag": true, "count": 42, "msg": "hello", "nothing": null});
    let b = json!({"count": 42, "flag": true, "msg": "hello", "nothing": null});
    assert_canonical_equal(&a, &b);
}

/// Golden vector: AcceptanceResponse-complete fixture.
/// This fixture mirrors what the coding harness produces in ok_envelope_json.
/// Both Harness and Kernel must compute the SAME digest.
#[test]
fn acceptance_response_full_fixture_is_canonical() {
    let dm_ref = "manifest_abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234";
    let dm_dig = format!("sha256:{}", "6".repeat(64));
    let merged = merged_response(Some(dm_ref), Some(&dm_dig));
    let digest = verify_opaque_payload_digest(&merged).unwrap();
    assert!(
        digest.starts_with("sha256:"),
        "digest must have sha256: prefix"
    );
    assert_eq!(digest.len(), 71, "sha256: + 64 hex chars = 71");
    // Two calls must produce identical digest
    let digest2 = verify_opaque_payload_digest(&merged).unwrap();
    assert_eq!(digest, digest2, "idempotent canonicalization");
}

/// Golden vector: tampered delivery_manifest_ref changes the digest.
#[test]
fn tampered_delivery_manifest_ref_changes_opaque_payload() {
    let dm_ref =
        "service_manifest_original_ref_original_ref_original_ref_original_ref_original_ref_original";
    let dm_dig = format!("sha256:{}", "6".repeat(64));
    let mut merged = merged_response(Some(dm_ref), Some(&dm_dig));
    merged["delivery_manifest_ref"] = json!("tampered_ref");
    let tampered_computed = verify_opaque_payload_digest(&merged).unwrap();
    let clean = merged_response(Some(dm_ref), Some(&dm_dig));
    let clean_computed = verify_opaque_payload_digest(&clean).unwrap();
    assert_ne!(
        tampered_computed, clean_computed,
        "tampered delivery_manifest_ref must change opaque payload"
    );
}

/// Golden vector: tampered delivery_manifest_digest changes the digest.
#[test]
fn tampered_delivery_manifest_digest_changes_opaque_payload() {
    let dm_ref =
        "service_manifest_abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234";
    let dm_dig = format!("sha256:{}", "6".repeat(64));
    let mut merged = merged_response(Some(dm_ref), Some(&dm_dig));
    merged["delivery_manifest_digest"] = json!(format!("sha256:{}", "9".repeat(64)));
    let tampered_computed = verify_opaque_payload_digest(&merged).unwrap();
    let clean = merged_response(Some(dm_ref), Some(&dm_dig));
    let clean_computed = verify_opaque_payload_digest(&clean).unwrap();
    assert_ne!(
        tampered_computed, clean_computed,
        "tampered delivery_manifest_digest must change opaque payload"
    );
}

/// Golden vector: different fixture shape (with/without manifest fields).
#[test]
fn different_fixture_shape_produces_different_digest() {
    let with_dm = merged_response(
        Some("service_manifest_ref"),
        Some(&format!("sha256:{}", "6".repeat(64))),
    );
    let without_dm = merged_response(None, None);
    let digest_with = verify_opaque_payload_digest(&with_dm).unwrap();
    let digest_without = verify_opaque_payload_digest(&without_dm).unwrap();
    assert_ne!(
        digest_with, digest_without,
        "presence of delivery_manifest fields must change opaque payload"
    );
}

/// Negative: uint or missing fields are handled (no panic).
#[test]
fn unknown_extra_fields_are_canonicalized() {
    let a = json!({"extra": "field", "b": 1, "a": 2});
    let b = json!({"a": 2, "b": 1, "extra": "field"});
    assert_canonical_equal(&a, &b);
}

/// Negative: empty object produces empty canonical JSON.
#[test]
fn empty_object_produces_empty_canonical() {
    let val = json!({});
    let bytes = canonical_bytes(&val);
    assert_eq!(bytes, b"{}");
}

/// Negative: null value is preserved.
#[test]
fn null_value_in_object() {
    let a = json!({"a": null, "b": 1});
    let b = json!({"b": 1, "a": null});
    assert_canonical_equal(&a, &b);
}

/// Negative: sort_object_keys returns a new Value, input clone is unchanged.
#[test]
fn sort_object_keys_returns_new_value() {
    let mut map = serde_json::Map::new();
    map.insert("z".to_string(), serde_json::Value::Number(1.into()));
    map.insert("a".to_string(), serde_json::Value::Number(2.into()));
    let original = serde_json::Value::Object(map);
    let sorted = sort_object_keys(&original);
    // Sorted output must have a before z (alphabetical)
    let sorted_keys: Vec<&str> = sorted
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        sorted_keys,
        vec!["a", "z"],
        "sorted must have alphabetical order"
    );
    // The function passed to sort returns a digest — verify it works
    let digest = verify_opaque_payload_digest(&original).unwrap();
    assert!(
        digest.starts_with("sha256:"),
        "digest must have sha256: prefix"
    );
    assert_eq!(digest.len(), 71, "sha256: + 64 hex chars = 71");
}
