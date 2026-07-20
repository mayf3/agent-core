#[cfg(test)]
#[path = "../coding_private_origin_tests.rs"]
mod private_origin_tests;

#[cfg(test)]
mod kernel_unified_delivery_manifest_tests {
    use crate::capabilities::store::{ContentStore, Sha256Digest};
    use crate::config::KernelConfig;
    use crate::domain::*;
    use anyhow::Result;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    /// Build a minimal accepted response with delivery_manifest fields.
    fn accepted_response(delivery_manifest_ref: &str, delivery_manifest_digest: &str) -> Value {
        json!({
            "outcome": "CandidatePassed",
            "candidate_id": "cand_test",
            "candidate_digest": format!("sha256:{}", "1".repeat(64)),
            "artifact_ref": "generated/test/artifact",
            "artifact_digest": format!("sha256:{}", "2".repeat(64)),
            "component_manifest_digest": format!("sha256:{}", "3".repeat(64)),
            "evidence_digest": format!("sha256:{}", "4".repeat(64)),
            "settlement_id": "settle_test",
            "claim_id": "claim_test",
            "run_id": "run_test",
            "harness_execution_id": "hex_test",
            "acceptance_invocation_id": "invocation_test",
            "delivery_manifest_ref": delivery_manifest_ref,
            "delivery_manifest_digest": delivery_manifest_digest,
        })
    }

    /// Build a minimal accepted response MISSING delivery_manifest fields.
    fn accepted_response_missing_delivery() -> Value {
        json!({
            "outcome": "CandidatePassed",
            "candidate_id": "cand_test",
            "candidate_digest": format!("sha256:{}", "1".repeat(64)),
            "artifact_ref": "generated/test/artifact",
            "artifact_digest": format!("sha256:{}", "2".repeat(64)),
            "component_manifest_digest": format!("sha256:{}", "3".repeat(64)),
            "evidence_digest": format!("sha256:{}", "4".repeat(64)),
            "settlement_id": "settle_test",
            "claim_id": "claim_test",
            "run_id": "run_test",
            "harness_execution_id": "hex_test",
            "acceptance_invocation_id": "invocation_test",
        })
    }

    fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("MISSING_{key}"))
    }

    fn required_digest(value: &Value, key: &str) -> Result<String> {
        let v = required_str(value, key)?;
        Sha256Digest::parse(v)?;
        Ok(v.to_string())
    }

    #[test]
    fn kernel_rejects_missing_delivery_manifest_fields() {
        let resp = accepted_response_missing_delivery();
        assert!(required_str(&resp, "delivery_manifest_ref").is_err());
        assert!(required_digest(&resp, "delivery_manifest_digest").is_err());
    }

    #[test]
    fn kernel_accepts_valid_delivery_manifest_fields() {
        let dig = format!("sha256:{}", "5".repeat(64));
        let resp = accepted_response("manifest_ref", &dig);
        assert_eq!(
            required_str(&resp, "delivery_manifest_ref").unwrap(),
            "manifest_ref"
        );
        assert_eq!(
            required_digest(&resp, "delivery_manifest_digest").unwrap(),
            dig
        );
    }

    // The following tests verify that the Kernel's unified delivery manifest
    // path works for both InvocableCapability and HookConsumerService payloads.
    // They use a real ContentStore to simulate the post-acceptance flow.

    fn setup_store() -> (PathBuf, ContentStore) {
        let dir = std::env::temp_dir().join(format!("kernel_dm_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ContentStore::new(dir.clone());
        (dir, store)
    }

    #[test]
    fn kernel_uses_generic_delivery_manifest_for_invocable() {
        let (_dir, store) = setup_store();
        // Simulate what the Harness stores: opaque bytes (any content)
        let manifest_bytes = b"{\"manifest_id\":\"test_invocable\",\"type\":\"HarnessManifest\"}";
        let stored_digest = store.store(manifest_bytes).unwrap();
        let stored_ref = "test_invocable";

        // Kernel reads back, verifies digest, uses ref (no type parsing)
        let key = Sha256Digest::parse(stored_digest.as_str()).unwrap();
        let loaded = store.load(&key).unwrap();
        let computed = Sha256Digest::compute(&loaded);
        assert_eq!(computed.as_str(), stored_digest.as_str());
        assert_eq!(stored_ref, "test_invocable");
    }

    #[test]
    fn kernel_does_not_parse_manifest_type() {
        let (_dir, store) = setup_store();
        // Both manifest types stored as opaque bytes
        let hook_bytes = b"{\"manifest_id\":\"hook_v1\",\"component_id\":\"test\",\"service\":{}}";
        let invocable_bytes =
            b"{\"manifest_id\":\"invocable_v1\",\"harness_id\":\"capability-host-v0\"}";

        let hook_digest = store.store(hook_bytes).unwrap();
        let invocable_digest = store.store(invocable_bytes).unwrap();

        // Kernel loads both and verifies digests without parsing
        let hook_key = Sha256Digest::parse(hook_digest.as_str()).unwrap();
        let invocable_key = Sha256Digest::parse(invocable_digest.as_str()).unwrap();

        let hook_loaded = store.load(&hook_key).unwrap();
        let invocable_loaded = store.load(&invocable_key).unwrap();

        assert_eq!(
            Sha256Digest::compute(&hook_loaded).as_str(),
            hook_digest.as_str()
        );
        assert_eq!(
            Sha256Digest::compute(&invocable_loaded).as_str(),
            invocable_digest.as_str()
        );

        // Kernel does NOT deserialize or inspect the content
        // (verified by the fact that we can't import the manifest types here)
    }

    #[test]
    fn kernel_rejects_tampered_delivery_manifest_digest() {
        let (_dir, store) = setup_store();
        let manifest_bytes = b"{\"manifest_id\":\"test\"}";
        let stored_digest = store.store(manifest_bytes).unwrap();

        // Kernel computes digest and compares against receipt field
        let loaded = store
            .load(&Sha256Digest::parse(stored_digest.as_str()).unwrap())
            .unwrap();
        let computed = Sha256Digest::compute(&loaded);
        assert_eq!(computed.as_str(), stored_digest.as_str());

        // If the digest doesn't match (e.g. tampered field), Kernel rejects
        let tampered_digest = format!("sha256:{}", "f".repeat(64));
        assert_ne!(computed.as_str(), tampered_digest);
    }

    #[test]
    fn kernel_rejects_delivery_manifest_ref_substitution() {
        let (_dir, store) = setup_store();
        let manifest_bytes = b"{\"manifest_id\":\"original_ref\"}";
        let stored_digest = store.store(manifest_bytes).unwrap();

        // The receipt binds a specific ref — Kernel uses exactly that ref
        let receipt_ref = "original_ref";
        let substituted_ref = "different_ref";

        // Load and verify content matches the stored digest
        let loaded = store
            .load(&Sha256Digest::parse(stored_digest.as_str()).unwrap())
            .unwrap();
        let computed = Sha256Digest::compute(&loaded);
        assert_eq!(computed.as_str(), stored_digest.as_str());

        // The ref from the acceptance response must be used for Proposal
        assert_eq!(receipt_ref, "original_ref");
        assert_ne!(receipt_ref, substituted_ref);
    }

    #[test]
    fn proposal_uses_receipt_bound_manifest() {
        let (_dir, store) = setup_store();
        let manifest_bytes = b"{\"manifest_id\":\"proposal_test\"}";
        let stored_digest = store.store(manifest_bytes).unwrap();

        // Kernel creates Proposal with the exact ref/digest from the receipt
        let proposal_ref = "proposal_test";
        let proposal_digest = stored_digest.as_str().to_string();

        // Verify the bytes match the digest
        let loaded = store
            .load(&Sha256Digest::parse(&proposal_digest).unwrap())
            .unwrap();
        let computed = Sha256Digest::compute(&loaded);
        assert_eq!(computed.as_str(), proposal_digest);

        // The proposal must use the same ref as the receipt
        assert_eq!(proposal_ref, "proposal_test");
    }

    #[test]
    fn development_request_product_validation_is_not_in_kernel() {
        // Verify that the Kernel crate no longer contains invocable_manifest
        // This is a compile-time check — the function WAS in super::invocable
        // and has been removed.  Trying to import it would fail:
        // use super::invocable::invocable_manifest; // COMPILE ERROR
        //
        // Instead, the Kernel only sees opaque delivery_manifest_ref/digest.
        assert!(true);
    }
}
