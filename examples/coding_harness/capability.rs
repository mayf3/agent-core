//! Capability proposal support for coding.capability.propose.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub fn handle_propose(args: &Value, root: Option<&PathBuf>) -> Value {
    let root = match root {
        Some(r) => r.clone(),
        None => return err("missing_workspace"),
    };

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

    if artifact_rel.is_empty() || manifest_rel.is_empty() || evidence_rel.is_empty() {
        return err("missing_path");
    }

    let artifact_path = root.join(artifact_rel);
    let manifest_path = root.join(manifest_rel);
    let evidence_path = root.join(evidence_rel);

    // Read and hash all three files.
    let artifact_data = match std::fs::read(&artifact_path) {
        Ok(d) => d,
        Err(e) => return err(&format!("artifact_read_failed: {e}")),
    };
    let manifest_data = match std::fs::read(&manifest_path) {
        Ok(d) => d,
        Err(e) => return err(&format!("manifest_read_failed: {e}")),
    };
    let evidence_data = match std::fs::read(&evidence_path) {
        Ok(d) => d,
        Err(e) => return err(&format!("evidence_read_failed: {e}")),
    };

    let artifact_digest = sha256_hex(&artifact_data);
    let manifest_content = String::from_utf8_lossy(&manifest_data);

    // Parse manifest JSON to extract operation details.
    let manifest_json: Value = match serde_json::from_str(&manifest_content) {
        Ok(v) => v,
        Err(e) => return err(&format!("manifest_parse_failed: {e}")),
    };

    let operation_name = manifest_json
        .get("operation_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if operation_name.is_empty() {
        return err("missing_operation_name");
    }

    // Compute the canonical manifest ID for this manifest.
    let canonical_json = serde_json::json!({
        "harness_id": manifest_json.get("harness_id"),
        "artifact_digest": format!("sha256:{artifact_digest}"),
        "protocol_version": manifest_json.get("protocol_version"),
        "endpoint": manifest_json.get("endpoint"),
        "operation_name": operation_name,
        "description": manifest_json.get("description"),
        "input_schema": manifest_json.get("input_schema"),
        "output_schema": manifest_json.get("output_schema"),
        "idempotent": manifest_json.get("idempotent"),
    });
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_string(&canonical_json)
            .unwrap_or_default()
            .as_bytes(),
    );
    let manifest_id = format!("manifest_{}", hex::encode(hasher.finalize()));

    ok(json!({
        "proposal": {
            "operation_name": operation_name,
            "manifest_id": manifest_id,
            "artifact_digest": format!("sha256:{artifact_digest}"),
            "manifest_size": manifest_data.len(),
            "artifact_size": artifact_data.len(),
            "evidence_size": evidence_data.len(),
        },
        "artifact_path": artifact_rel,
        "manifest_path": manifest_rel,
        "evidence_path": evidence_rel,
        "status": "pending_proposal",
    }))
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn ok(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
fn err(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}
