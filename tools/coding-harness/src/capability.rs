use crate::config::CodingConfig;
use crate::paths::{assert_no_symlink_escape, resolve_path_unchecked, validate_relative};
use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::harness::manifest::HarnessManifest;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const MAX_ARTIFACT_SIZE: usize = 2 * 1024 * 1024;
const MAX_MANIFEST_SIZE: usize = 256 * 1024;
const MAX_EVIDENCE_SIZE: usize = 256 * 1024;

fn err(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}

fn structured_err(code: &str, missing: &[&str], available_ids: &[String]) -> Value {
    let mut details = json!({});
    if !missing.is_empty() {
        details["missing_fields"] = json!(missing);
    }
    if !available_ids.is_empty() {
        details["available_workspace_ids"] = json!(available_ids);
    }
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
        "retryable": true,
        "details": details,
    })
}

pub fn handle_propose(root: &Path, args: &Value, config: &CodingConfig) -> Value {
    let artifact_rel = args
        .get("artifact_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let manifest_rel = args
        .get("manifest_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let evidence_rel = args
        .get("evidence_path")
        .and_then(Value::as_str)
        .unwrap_or("");

    // Build available workspace IDs for structured error messages.
    let ws_ids: Vec<String> = config.workspaces.keys().cloned().collect();

    if artifact_rel.is_empty() || manifest_rel.is_empty() || evidence_rel.is_empty() {
        let mut missing = Vec::new();
        if artifact_rel.is_empty() {
            missing.push("artifact_path");
        }
        if manifest_rel.is_empty() {
            missing.push("manifest_path");
        }
        if evidence_rel.is_empty() {
            missing.push("evidence_path");
        }
        return structured_err("missing_path", &missing, &ws_ids);
    }

    // Validate paths (reject absolute, .., symlink escape)
    for rel in &[artifact_rel, manifest_rel, evidence_rel] {
        if let Err(e) = validate_relative(rel) {
            return err(&format!("invalid_path: {e}"));
        }
    }

    let artifact_path = match resolve_path_unchecked(root, artifact_rel) {
        Ok(p) => p,
        Err(e) => return err(&format!("artifact_resolve_failed: {e}")),
    };
    let manifest_path = match resolve_path_unchecked(root, manifest_rel) {
        Ok(p) => p,
        Err(e) => return err(&format!("manifest_resolve_failed: {e}")),
    };
    let evidence_path = match resolve_path_unchecked(root, evidence_rel) {
        Ok(p) => p,
        Err(e) => return err(&format!("evidence_resolve_failed: {e}")),
    };

    // Check symlink escape for all three
    for p in [&artifact_path, &manifest_path, &evidence_path] {
        if let Err(e) = assert_no_symlink_escape(root, p) {
            return err(&format!("symlink_escape: {e}"));
        }
    }

    // Read files
    let artifact_data = match bounded_read(&artifact_path, MAX_ARTIFACT_SIZE) {
        Ok(d) => d,
        Err(e) => return err(&format!("artifact_read_failed: {e}")),
    };
    let manifest_raw = match bounded_read(&manifest_path, MAX_MANIFEST_SIZE) {
        Ok(d) => d,
        Err(e) => return err(&format!("manifest_read_failed: {e}")),
    };
    let evidence_data = match bounded_read(&evidence_path, MAX_EVIDENCE_SIZE) {
        Ok(d) => d,
        Err(e) => return err(&format!("evidence_read_failed: {e}")),
    };

    // Compute digests
    let artifact_digest = Sha256Digest::compute(&artifact_data);

    let manifest_value: Value = match serde_json::from_slice(&manifest_raw) {
        Ok(v) => v,
        Err(e) => return err(&format!("manifest_parse_failed: {e}")),
    };

    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: manifest_value
            .get("harness_id")
            .and_then(Value::as_str)
            .unwrap_or("coding_harness")
            .to_string(),
        artifact_digest: artifact_digest.as_str().to_string(),
        protocol_version: manifest_value
            .get("protocol_version")
            .and_then(Value::as_str)
            .unwrap_or("external-harness-v1")
            .to_string(),
        endpoint: manifest_value
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        operation_name: manifest_value
            .get("operation_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: manifest_value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        input_schema: manifest_value.get("input_schema").cloned().unwrap_or(
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        ),
        output_schema: manifest_value.get("output_schema").cloned().unwrap_or(
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        ),
        idempotent: manifest_value
            .get("idempotent")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        created_at: Utc::now(),
    };

    let manifest_id = match manifest.compute_manifest_id() {
        Ok(id) => id,
        Err(e) => return err(&format!("manifest_id_failed: {e}")),
    };
    manifest.manifest_id = manifest_id;

    let final_manifest_bytes = match serde_json::to_vec(&manifest) {
        Ok(b) => b,
        Err(e) => return err(&format!("manifest_serialize_failed: {e}")),
    };

    // Store in ContentStore
    let store = ContentStore::new(config.artifact_root.clone());
    let (stored_artifact_digest, stored_manifest_digest, stored_evidence_digest) = match (
        store.store(&artifact_data),
        store.store(&final_manifest_bytes),
        store.store(&evidence_data),
    ) {
        (Ok(a), Ok(m), Ok(e)) => (a, m, e),
        (Err(e), _, _) => return err(&format!("store_artifact_failed: {e}")),
        (_, Err(e), _) => return err(&format!("store_manifest_failed: {e}")),
        (_, _, Err(e)) => return err(&format!("store_evidence_failed: {e}")),
    };

    // Build proposal body for the kernel API
    let target_agent = manifest_value
        .get("target_agent_id")
        .and_then(Value::as_str)
        .unwrap_or("main");
    let risk = manifest_value
        .get("risk_summary")
        .and_then(Value::as_str)
        .unwrap_or("read-only");

    let submit_body = json!({
        "target_agent_id": target_agent,
        "artifact_ref": artifact_rel,
        "artifact_digest": stored_artifact_digest.as_str(),
        "manifest_ref": manifest_rel,
        "manifest_digest": stored_manifest_digest.as_str(),
        "evidence_ref": evidence_rel,
        "evidence_digest": stored_evidence_digest.as_str(),
        "requested_operations": [manifest.operation_name],
        "risk_summary": risk,
    });

    // Submit via HTTP to the Kernel's proposal API
    let api_url = format!(
        "{}/v1/capability-change-proposals",
        config.kernel_api_url.trim_end_matches('/')
    );

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent();

    let response = agent
        .post(&api_url)
        .header(
            "Authorization",
            &format!("Bearer {}", config.capability_submit_token),
        )
        .header("Content-Type", "application/json")
        .send_json(submit_body);

    let mut resp = match response {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) => {
            return err(&format!("kernel_api_error_{}", code));
        }
        Err(e) => return err(&format!("kernel_api_failed: {e}")),
    };
    let response_body: Value = match resp.body_mut().read_json::<Value>() {
        Ok(v) => v,
        Err(e) => return err(&format!("kernel_api_json_failed: {e}")),
    };

    ok(json!({
        "proposal_id": response_body.get("proposal_id"),
        "status": response_body.get("status"),
        "expected_active_snapshot_id": response_body.get("expected_active_snapshot_id"),
        "requested_operations": response_body.get("requested_operations"),
        "expires_at": response_body.get("expires_at"),
        "artifact_digest": stored_artifact_digest.as_str(),
        "manifest_digest": stored_manifest_digest.as_str(),
        "evidence_digest": stored_evidence_digest.as_str(),
        "manifest_id": manifest.manifest_id,
        "operation_name": manifest.operation_name,
    }))
}

fn bounded_read(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{e}"))?;
    let meta = f.metadata().map_err(|e| format!("{e}"))?;
    if meta.len() > max as u64 {
        return Err(format!("file_too_large: {} bytes", meta.len()));
    }
    let mut data = Vec::with_capacity((meta.len() as usize).min(max));
    f.read_to_end(&mut data).map_err(|e| format!("{e}"))?;
    Ok(data)
}

fn ok(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
