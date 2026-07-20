//! Tests for HCR acceptance opaque payload verification.

use super::harness_client::verify_opaque_payload_digest;
use serde_json::json;

/// Build a merged response value (AcceptanceResponse + envelope fields)
/// as produced by the Harness's `ok_envelope_json()`.
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

#[test]
fn delivery_manifest_digest_is_opaque_payload_bound() {
    let dm_ref =
        "service_manifest_abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234";
    let dm_dig = format!("sha256:{}", "6".repeat(64));
    let merged = merged_response(Some(dm_ref), Some(&dm_dig));
    let computed = verify_opaque_payload_digest(&merged).unwrap();
    assert!(
        computed.starts_with("sha256:"),
        "opaque payload must be a valid sha256 digest"
    );
    assert_eq!(computed.len(), 71, "sha256: + 64 hex chars");
}

#[test]
fn tampered_delivery_manifest_ref_is_rejected() {
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

#[test]
fn tampered_delivery_manifest_digest_is_rejected() {
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

#[test]
fn delivery_manifest_fields_are_part_of_opaque_payload() {
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
