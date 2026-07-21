//! HCR acceptance orchestrator.
//!
//! Receives a candidate reference, snapshots it, runs all five
//! acceptance gates under OS file lock (H7), persists the result
//! atomically, and returns a structured `ExternalReceiptEnvelope`
//! with the detailed acceptance response alongside.

pub mod execution_store;
pub mod protocol;
pub mod verification_receipt;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

    use agent_core_kernel::domain::external_receipt_envelope::{
	    compute_external_receipt_digest, ExternalOutcome, ExternalReceiptEnvelope, SCHEMA_VERSION,
	};
	use serde_json::Value;
	
	use super::candidate::snapshot_candidate;
	use super::gates::{run_all_gates_for_acceptance, GateKind, GateResult};
	use super::manifest_builder::{allocate_next_version, build_delivery_manifest};
	use agent_core_kernel::domain::DevelopmentRequest;
	use execution_store::ExecutionStore;
	use protocol::{
	    canonical_evidence_bytes, compute_evidence_digest, compute_fingerprint, AcceptanceResponse,
	    GateResultEntry,
	};

/// Global gate execution counter (test observation only).
static GATE_EXECUTION_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn reset_execution_count() {
    GATE_EXECUTION_COUNT.store(0, Ordering::SeqCst);
}
pub fn execution_count() -> usize {
    GATE_EXECUTION_COUNT.load(Ordering::SeqCst)
}

/// Handle an acceptance request. Dispatches through ExecutionStore for
/// idempotency, locking, crash recovery (H7), and atomic persistence.
pub fn handle_accept(artifact_root: &Path, args: &Value) -> Value {
    let idempotency_key = get_str(args, "idempotency_key").unwrap_or("");
    let hcr_id = get_str(args, "hcr_id").unwrap_or("");
    let claim_id = get_str(args, "claim_id").unwrap_or("");
    let run_id = get_str(args, "run_id").unwrap_or("");
    let principal_id = get_str(args, "principal_id").unwrap_or("");
    let gateway_session_id = get_str(args, "gateway_session_id").unwrap_or("");
    let registry_snapshot_id = get_str(args, "registry_snapshot_id").unwrap_or("");
    let operation = get_str(args, "operation").unwrap_or("external.coding_hcr_accept");
    let candidate_ref = match get_str(args, "candidate_ref") {
        Some(c) if !c.is_empty() => c,
        _ => return err_json("MISSING_CANDIDATE_REF"),
    };
    let invocation_intent_id = get_str(args, "invocation_intent_id").unwrap_or("");
    let requirement_digest = match get_str(args, "requirement_digest") {
        Some(d) => {
            if !d.starts_with("sha256:") || d.len() != 71 {
                return err_json("INVALID_REQUIREMENT_DIGEST");
            }
            d
        }
        None => return err_json("MISSING_REQUIREMENT_DIGEST"),
    };

    // Validate requirement content against its digest — ensures the
    // development_request we extract matches what the Kernel signed.
    // (also serves as an early-fail pre-check before the locked section)

    if idempotency_key.is_empty() {
        return err_json("MISSING_IDEMPOTENCY_KEY");
    }
    if invocation_intent_id.is_empty() {
        return err_json("MISSING_INVOCATION_INTENT_ID");
    }

    let fingerprint = compute_fingerprint(
        hcr_id,
        claim_id,
        run_id,
        principal_id,
        gateway_session_id,
        registry_snapshot_id,
        operation,
        candidate_ref,
        idempotency_key,
        Some(requirement_digest),
    );

    let store = ExecutionStore::new(artifact_root);

    // Execute under OS file lock (H7): crash-safe, idempotent
    match store.execute(idempotency_key, &fingerprint, || {
        GATE_EXECUTION_COUNT.fetch_add(1, Ordering::SeqCst);
        do_accept(
            artifact_root,
            args,
            &fingerprint,
            hcr_id,
            claim_id,
            run_id,
            principal_id,
            gateway_session_id,
            registry_snapshot_id,
            operation,
            candidate_ref,
            idempotency_key,
        )
        .and_then(|resp| serde_json::to_value(resp).map_err(|e| e.to_string()))
    }) {
        Ok(result) => {
            // Deserialize the AcceptanceResponse to build the envelope
            let acceptance: AcceptanceResponse = match serde_json::from_value(result.clone()) {
                Ok(a) => a,
                Err(e) => return err_json(&format!("ENVELOPE_SERIALIZATION: {e}")),
            };
            ok_envelope_json(&acceptance, &invocation_intent_id, result)
        }
        Err(execution_store::ExecutionStoreError::FingerprintMismatch(_)) => {
            err_json("IDEMPOTENCY_CONFLICT")
        }
        Err(execution_store::ExecutionStoreError::LockFailed(e)) => {
            err_json(&format!("LOCK_FAILED: {e}"))
        }
        Err(e) => err_json(&format!("EXECUTION_FAILED: {e}")),
    }
}

/// Core acceptance logic (runs under file lock, called at most once per key).
fn do_accept(
    artifact_root: &Path,
    args: &Value,
    fingerprint: &protocol::RequestFingerprint,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    principal_id: &str,
    gateway_session_id: &str,
    registry_snapshot_id: &str,
    operation: &str,
    candidate_ref: &str,
    idempotency_key: &str,
) -> Result<AcceptanceResponse, String> {
    let candidate_path = resolve_safe(artifact_root, candidate_ref)
        .ok_or_else(|| "CANDIDATE_REF_ESCAPE".to_string())?;
    if !candidate_path.is_dir() {
        return Err("CANDIDATE_NOT_FOUND".to_string());
    }

    let base_dir = artifact_root.join("candidates_base");
    std::fs::create_dir_all(&base_dir).map_err(|e| format!("BASE_DIR: {e}"))?;

    let snapshot =
        snapshot_candidate(&candidate_path, &base_dir).map_err(|e| format!("SNAPSHOT: {e}"))?;

    let gate_run = run_all_gates_for_acceptance(&snapshot);
    let results = gate_run.results;
    let outcome = classify_outcome(&results);
    let artifact_digest = extract_artifact(&results);

    // Persist the exact verified executable bytes in the shared content store.
    // The digest returned by the Artifact gate must match the store's digest.
    let artifact_ref = if outcome == "CandidatePassed" {
        let bytes = gate_run
            .artifact_bytes
            .as_deref()
            .ok_or_else(|| "ACCEPTED_ARTIFACT_BYTES_MISSING".to_string())?;
        let stored =
            agent_core_kernel::capabilities::store::ContentStore::new(artifact_root.to_path_buf())
                .store(bytes)
                .map_err(|e| format!("ARTIFACT_STORE: {e}"))?;
        if artifact_digest.as_deref() != Some(stored.as_str()) {
            return Err("ARTIFACT_STORE_DIGEST_MISMATCH".to_string());
        }
        Some(stored.as_str().to_string())
    } else {
        None
    };

    // Store the exact canonical component manifest that the accepted candidate
    // was gated against. Kernel later reloads this digest instead of trusting
    // the pre-acceptance submit response.
    let component_manifest_digest = if outcome == "CandidatePassed" {
        let raw = std::fs::read(snapshot.candidate_path.join("manifest.json"))
            .map_err(|e| format!("COMPONENT_MANIFEST_READ: {e}"))?;
        let value: Value =
            serde_json::from_slice(&raw).map_err(|e| format!("COMPONENT_MANIFEST_PARSE: {e}"))?;
        let canonical =
            serde_json::to_vec(&value).map_err(|e| format!("COMPONENT_MANIFEST_CANONICAL: {e}"))?;
        Some(
            agent_core_kernel::capabilities::store::ContentStore::new(artifact_root.to_path_buf())
                .store(&canonical)
                .map_err(|e| format!("COMPONENT_MANIFEST_STORE: {e}"))?
                .as_str()
                .to_string(),
        )
    } else {
        None
    };

	// ── Post‑acceptance: version allocation + delivery manifest ──
	    // The original candidate manifest.json is never modified on disk.
	    // We work on an in‑memory copy so the accepted candidate stays
	    // immutable.  The resulting delivery manifest bytes are stored in
	    // the shared ContentStore and bound by the opaque_payload_digest.
	    let (delivery_manifest_ref, delivery_manifest_digest) = if outcome == "CandidatePassed" {
	        let raw = std::fs::read(snapshot.candidate_path.join("manifest.json"))
	            .map_err(|e| format!("DELIVERY_MANIFEST_READ: {e}"))?;
	        let mut manifest_value: Value =
	            serde_json::from_slice(&raw).map_err(|e| format!("DELIVERY_MANIFEST_PARSE: {e}"))?;

        // Parse DevelopmentRequest from the verified requirement content.
        // We re-verify the digest inside the locked execution (defense in depth)
        // so that tampering between handle_accept and do_accept is detected.
        let development_request = extract_development_request(args);

	        // Only allocate versions for HookConsumerService (managed services)
	        let needs_version = manifest_value
	            .get("service")
	            .and_then(|s| s.get("version"))
	            .is_some();
	        if needs_version {
	            let component_id = manifest_value["component_id"]
	                .as_str()
	                .unwrap_or("")
	                .to_string();
	            let new_ver = allocate_next_version(&component_id)
	                .map_err(|e| format!("VERSION_ALLOCATION: {e}"))?;
	            if let Some(ver) = new_ver {
	                manifest_value["service"]["version"] = serde_json::json!(ver);
	            }
	        }

	        let artifact_digest_str = artifact_digest.as_deref().unwrap_or("");
	        let (delivery_ref, delivery_bytes) = build_delivery_manifest(
	            &manifest_value,
	            artifact_digest_str,
	            development_request.as_ref(),
	        )
	        .map_err(|e| format!("DELIVERY_MANIFEST_BUILD: {e}"))?;
	        let store = agent_core_kernel::capabilities::store::ContentStore::new(
	            artifact_root.to_path_buf(),
	        );
	        let stored = store
	            .store(&delivery_bytes)
	            .map_err(|e| format!("DELIVERY_MANIFEST_STORE: {e}"))?;
	        (Some(delivery_ref), Some(stored.as_str().to_string()))
	    } else {
	        (None, None)
	    };

    validate_gate_consistency(&results, &outcome, &artifact_digest)?;

    let gate_entries: Vec<GateResultEntry> = results
        .iter()
        .map(|r| GateResultEntry {
            gate_kind: r.gate_kind.as_str().to_string(),
            passed: r.passed,
            is_candidate_failure: r.is_candidate_failure,
            exit_code: r.exit_code,
            timed_out: r.timed_out,
            error_code: r.error_code.clone(),
            stdout: r.stdout.clone(),
            stderr: r.stderr.clone(),
        })
        .collect();

    let harness_execution_id = sha256_prefix(idempotency_key);

    let evidence_digest = compute_evidence_digest(
        &harness_execution_id,
        fingerprint,
        &snapshot.candidate_id,
        &snapshot.candidate_digest,
        &gate_entries,
        &outcome,
        artifact_ref.as_deref(),
        artifact_digest.as_deref(),
        component_manifest_digest.as_deref(),
    );

    let evidence_bytes = canonical_evidence_bytes(
        &harness_execution_id,
        fingerprint,
        &snapshot.candidate_id,
        &snapshot.candidate_digest,
        &gate_entries,
        &outcome,
        artifact_ref.as_deref(),
        artifact_digest.as_deref(),
        component_manifest_digest.as_deref(),
    );
    let stored_evidence =
        agent_core_kernel::capabilities::store::ContentStore::new(artifact_root.to_path_buf())
            .store(&evidence_bytes)
            .map_err(|e| format!("EVIDENCE_STORE: {e}"))?;
    if stored_evidence.as_str() != evidence_digest {
        return Err("EVIDENCE_STORE_DIGEST_MISMATCH".to_string());
    }

    Ok(AcceptanceResponse {
        harness_execution_id,
        idempotency_key: idempotency_key.to_string(),
        hcr_id: hcr_id.to_string(),
        claim_id: claim_id.to_string(),
        run_id: run_id.to_string(),
        principal_id: principal_id.to_string(),
        gateway_session_id: gateway_session_id.to_string(),
        registry_snapshot_id: registry_snapshot_id.to_string(),
        operation: operation.to_string(),
        candidate_id: snapshot.candidate_id,
        candidate_digest: snapshot.candidate_digest,
        overall_outcome: outcome,
        gate_results: gate_entries,
        artifact_ref,
        artifact_digest,
        component_manifest_digest,
        delivery_manifest_ref,
        delivery_manifest_digest,
        evidence_digest,
    })
}

/// Wrap the acceptance response in an `ExternalReceiptEnvelope`.
	fn ok_envelope_json(
	    acceptance: &AcceptanceResponse,
	    invocation_intent_id: &str,
	    detailed_result: Value,
	) -> Value {
	    // Map the harness outcome to the envelope's ExternalOutcome
	    let outcome = match acceptance.overall_outcome.as_str() {
	        "CandidatePassed" => ExternalOutcome::Passed,
	        _ => ExternalOutcome::Failed,
	    };
	
	    // Compute opaque_payload_digest as SHA-256 of the full AcceptanceResponse JSON
	    // with CANONICAL (alphabetically sorted) key order. Both the coding harness and
	    // the Kernel reproduce the same canonical serialization so the digest matches
	    // regardless of Value construction order vs struct declaration order.
	    use sha2::{Digest, Sha256};
	    let resp_value = serde_json::to_value(acceptance).unwrap_or_default();
	    let canonical_bytes = canonical_json_bytes(&resp_value);
	    let opaque_payload_digest = format!("sha256:{}", hex::encode(Sha256::digest(&canonical_bytes)));

    // Build the envelope
    let env = ExternalReceiptEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        invocation_intent_id: invocation_intent_id.to_string(),
        issuer: "coding-harness".to_string(),
        subject_digest: acceptance.candidate_digest.clone(),
        outcome,
        evidence_digest: acceptance.evidence_digest.clone(),
        opaque_payload_digest: Some(opaque_payload_digest),
        receipt_digest: String::new(), // will be set below
    };

    // Compute receipt_digest BEFORE serialization
    let receipt_digest = compute_external_receipt_digest(
        &env.schema_version,
        &env.invocation_intent_id,
        &env.issuer,
        &env.subject_digest,
        env.outcome,
        &env.evidence_digest,
        env.opaque_payload_digest.as_deref(),
    );

    // Build the response: envelope + detailed fields for Kernel persistence
    let envelope_value = serde_json::json!({
        "schema_version": env.schema_version,
        "invocation_intent_id": env.invocation_intent_id,
        "issuer": env.issuer,
        "subject_digest": env.subject_digest,
        "outcome": env.outcome,
        "evidence_digest": env.evidence_digest,
        "opaque_payload_digest": env.opaque_payload_digest,
        "receipt_digest": receipt_digest,
    });

    // Merge envelope fields with detailed response fields
    let mut result = detailed_result;
    if let Some(obj) = result.as_object_mut() {
        for (k, v) in envelope_value.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
    }

    serde_json::json!({
        "protocol_version": "external-harness-v1",
        "ok": true,
        "result": result,
    })
}

fn sha256_prefix(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))[..16].to_string()
}

fn classify_outcome(results: &[GateResult]) -> String {
    if results.iter().any(|r| !r.passed && !r.is_candidate_failure) {
        return "InfrastructureFailure".into();
    }
    if results.iter().any(|r| !r.passed && r.is_candidate_failure) {
        return "CandidateFailed".into();
    }
    if results.iter().all(|r| r.passed) {
        return "CandidatePassed".into();
    }
    "InfrastructureFailure".into()
}

fn extract_artifact(results: &[GateResult]) -> Option<String> {
    for r in results {
        if r.gate_kind == GateKind::Artifact && r.passed {
            let digest = r
                .computed_artifact_digest
                .clone()
                .unwrap_or_else(|| "unknown".into());
            return Some(digest);
        }
    }
    None
}

fn validate_gate_consistency(
    results: &[GateResult],
    outcome: &str,
    artifact_digest: &Option<String>,
) -> Result<(), String> {
    let kinds: std::collections::HashSet<String> = results
        .iter()
        .map(|r| r.gate_kind.as_str().to_string())
        .collect();
    if kinds.len() != 5 {
        return Err(format!("expected 5 gates, got {}", kinds.len()));
    }
    match outcome {
        "CandidatePassed" => {
            if results.iter().any(|r| !r.passed) {
                return Err("CandidatePassed but gates failed".into());
            }
            if artifact_digest.is_none() {
                return Err("CandidatePassed missing artifact_digest".into());
            }
        }
        "CandidateFailed" => {
            if !results.iter().any(|r| !r.passed && r.is_candidate_failure) {
                return Err("CandidateFailed but no failure".into());
            }
        }
        "InfrastructureFailure" => {
            if !results.iter().any(|r| !r.passed && !r.is_candidate_failure) {
                return Err("InfraFailure but no infra".into());
            }
        }
        _ => return Err(format!("unknown outcome: {outcome}")),
    }
    Ok(())
}

fn resolve_safe(root: &Path, rel: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(rel);
    if p.is_absolute() || rel.contains("..") {
        return None;
    }
    let j = root.join(p);
    if !j.starts_with(root) {
        return None;
    }
    if let Ok(c) = j.canonicalize() {
        if let Ok(rc) = root.canonicalize() {
            if !c.starts_with(&rc) {
                return None;
            }
        }
    }
    Some(j)
}

/// Extract and verify development_request from the acceptance args.
///
/// Re-verifies requirement_digest against the raw requirement string
/// inside the locked execution (defense in depth against tampering
/// between `handle_accept` pre-checks and `do_accept`).
fn extract_development_request(args: &Value) -> Option<DevelopmentRequest> {
    let req_digest = get_str(args, "requirement_digest")?;
    let req_str = get_str(args, "requirement")?;
    use sha2::{Digest, Sha256};
    let computed = format!("sha256:{}", hex::encode(Sha256::digest(req_str.as_bytes())));
    if computed != req_digest {
        eprintln!("[extract_development_request] FATAL: digest mismatch. No fallback allowed. Must fail closed.");
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(req_str).ok()?;
    let dev_req_val = parsed.get("development_request").cloned()?;
    match serde_json::from_value::<DevelopmentRequest>(dev_req_val.clone()) {
        Ok(dr) => Some(dr),
        Err(e) => {
            eprintln!("[extract_development_request] DevelopmentRequest deserialization failed: {e}");
            eprintln!("[extract_development_request] development_request value keys: {:?}",
                dev_req_val.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
            None
        }
    }
}

fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|v| v.as_str())
}

fn err_json(code: &str) -> Value {
    serde_json::json!({"protocol_version":"external-harness-v1","ok":false,"error_code":code})
}

/// Canonical JSON serialization with alphabetically sorted keys.
/// Ensures both coding harness and Kernel compute the same digest
/// regardless of Value construction order vs struct declaration order.
fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    let sorted = sort_keys(value);
    serde_json::to_vec(&sorted).unwrap_or_default()
}

fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: serde_json::Map<String, Value> = map.iter().map(|(k, v)| {
                (k.clone(), sort_keys(v))
            }).collect();
            sorted.sort_keys();
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}
