use super::*;

fn valid_manifest() -> serde_json::Value {
    serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "my_harness",
        "bundle_version": "1.0.0",
        "operations": [{
            "name": "my_op",
            "description": "my harness operation",
            "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": false},
            "risk": "ReadOnly",
            "idempotent": true
        }]
    })
}

#[test]
fn valid_manifest_passes_validation() {
    let result = validate_manifest(&valid_manifest(), None);
    assert!(result.is_ok(), "valid manifest: {:?}", result.err());
}

#[test]
fn declared_hash_matches() {
    let manifest = validate_manifest(&valid_manifest(), None).unwrap();
    let hash = compute_bundle_hash(&manifest);
    let result = validate_manifest(&valid_manifest(), Some(&hash));
    assert!(result.is_ok());
}

#[test]
fn declared_hash_mismatch_rejected() {
    let result = validate_manifest(&valid_manifest(), Some("sha256:badhash"));
    assert!(result.is_err());
    let err = result.err().unwrap().to_string();
    assert!(err.contains("declared bundle hash") || err.contains("does not match"));
}

#[test]
fn manifest_version_must_be_v1() {
    let mut m = valid_manifest();
    m["manifest_version"] = serde_json::json!("v2");
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn protocol_version_must_be_v1() {
    let mut m = valid_manifest();
    m["protocol_version"] = serde_json::json!("v2");
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn operations_must_not_be_empty() {
    let mut m = valid_manifest();
    m["operations"] = serde_json::json!([]);
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn duplicate_operation_names_rejected() {
    let mut m = valid_manifest();
    m["operations"] = serde_json::json!([
        {"name": "my_op", "description": "first", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
        {"name": "my_op", "description": "second", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
    ]);
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn risk_must_be_readonly() {
    let mut m = valid_manifest();
    m["operations"][0]["risk"] = serde_json::json!("Write");
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn idempotent_must_be_true() {
    let mut m = valid_manifest();
    m["operations"][0]["idempotent"] = serde_json::json!(false);
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn operation_name_illegal_chars_rejected() {
    let mut m = valid_manifest();
    m["operations"][0]["name"] = serde_json::json!("illegal space");
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn bundle_id_must_be_valid() {
    let mut m = valid_manifest();
    m["bundle_id"] = serde_json::json!("");
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn bundle_version_must_not_be_empty() {
    let mut m = valid_manifest();
    m["bundle_version"] = serde_json::json!("");
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn unsupported_schema_keyword_rejected() {
    let mut m = valid_manifest();
    m["operations"][0]["parameters"] =
        serde_json::json!({"type": "object", "$ref": "#/definitions/X"});
    assert!(validate_manifest(&m, None).is_err());
}

#[test]
fn canonicalization_normalizes_json_order() {
    let m1 = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "harness",
        "bundle_version": "1.0",
        "operations": [{"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
    });
    let m2 = serde_json::json!({
        "protocol_version": "v1",
        "bundle_id": "harness",
        "bundle_version": "1.0",
        "manifest_version": "v1",
        "operations": [{"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}]
    });
    let manifest1 = validate_manifest(&m1, None).unwrap();
    let manifest2 = validate_manifest(&m2, None).unwrap();
    let hash1 = compute_bundle_hash(&manifest1);
    let hash2 = compute_bundle_hash(&manifest2);
    assert_eq!(hash1, hash2, "different key order must produce same hash");
}

#[test]
fn operations_order_does_not_affect_hash() {
    let m1 = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "harness",
        "bundle_version": "1.0",
        "operations": [
            {"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
            {"name": "op_b", "description": "b", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}
        ]
    });
    let m2 = serde_json::json!({
        "manifest_version": "v1",
        "protocol_version": "v1",
        "bundle_id": "harness",
        "bundle_version": "1.0",
        "operations": [
            {"name": "op_b", "description": "b", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true},
            {"name": "op_a", "description": "a", "parameters": {"type": "object"}, "risk": "ReadOnly", "idempotent": true}
        ]
    });
    let manifest1 = validate_manifest(&m1, None).unwrap();
    let manifest2 = validate_manifest(&m2, None).unwrap();
    let hash1 = compute_bundle_hash(&manifest1);
    let hash2 = compute_bundle_hash(&manifest2);
    assert_eq!(
        hash1, hash2,
        "different operation order must produce same hash"
    );
}

#[test]
fn hash_is_deterministic() {
    let manifest = validate_manifest(&valid_manifest(), None).unwrap();
    let h1 = compute_bundle_hash(&manifest);
    let h2 = compute_bundle_hash(&manifest);
    assert_eq!(h1, h2);
}

#[test]
fn prepare_operation_sets_external_harness_binding() {
    let manifest = validate_manifest(&valid_manifest(), None).unwrap();
    let hash = compute_bundle_hash(&manifest);
    let prepared = prepare_operation(&manifest.operations[0], &hash);
    assert_eq!(prepared.spec.binding_kind, BindingKind::ExternalHarness);
    assert!(prepared.spec.binding_key.starts_with("harness:"));
    assert!(prepared.spec.binding_key.contains(&hash));
}

#[test]
fn declared_hash_stripped_by_canonicalization() {
    // declared_hash is consumed by validation, not stored in manifest
    let manifest = validate_manifest(&valid_manifest(), None).unwrap();
    assert!(serde_json::to_string(&manifest).unwrap().contains("my_op"));
    // The canonical manifest should not contain 'bundle_hash' field
    let canonical = serde_json::to_string(&manifest).unwrap();
    assert!(
        !canonical.contains("bundle_hash"),
        "bundle_hash should not be in canonical manifest"
    );
}
