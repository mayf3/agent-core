//! Frozen calculator fixture for the generic invocable-capability profile.

use agent_core_kernel::domain::{DevelopmentRequest, TargetKind};
use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

const CARGO_TOML: &str = r#"[package]
name = "calculator-harness"
version = "0.1.0"
edition = "2021"

# stdlib-only so the five acceptance gates can build offline
"#;

const CANDIDATE_MANIFEST: &str = r#"{
  "schema_version": "component-artifact-v1",
  "component_id": "external.calculator",
  "kind": "invocable_capability",
  "profile_id": "invocable-capability-v0",
  "contract_catalog_version": "contract-catalog-v1",
  "required_contracts": ["component.invoke.v0"],
  "requested_permissions": ["component.invoke"],
  "test_kit": "calculator-fixture-v0",
  "deployment_profile": "capability-host-v0",
  "runtime_profile": "process-harness-v1",
  "healthcheck": "trusted process invocation",
  "rollback_policy": "reactivate previous content-addressed snapshot",
  "manifest_id": "calculator-v0-candidate",
  "harness_id": "calculator-harness",
  "protocol_version": "external-harness-v1",
  "operation": "external.calculator",
  "operations": ["add", "subtract", "multiply", "divide"],
  "description": "Calculator supporting add, subtract, multiply, and divide.",
  "entry": "target/release/calculator-harness",
  "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
  "capability": {
    "operation_name": "external.calculator",
    "description": "Approved calculator supporting add, subtract, multiply, and divide.",
    "input_schema": {
      "type": "object",
      "properties": {
        "operation": {"type": "string", "enum": ["add", "subtract", "multiply", "divide"]},
        "a": {"type": "number"},
        "b": {"type": "number"}
      },
      "required": ["operation", "a", "b"],
      "additionalProperties": false
    },
    "output_schema": {"type": "number"},
    "idempotent": true
  }
}
"#;

const MAIN_RS: &str = r#"use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(1);
    }
    let protocol = string_field(&input, "protocol_version")
        .or_else(|| string_field(&input, "protocol"))
        .unwrap_or_default();
    let operation = string_field(&input, "operation")
        .or_else(|| string_field(&input, "operation_name"))
        .unwrap_or_default();
    if protocol != "process-harness-v1" {
        respond_error("unsupported_protocol");
        return;
    }
    if operation == "__agent_core_describe" {
        let _ = writeln!(std::io::stdout(), "{{\"ok\":true,\"result\":{{\"descriptor_version\":\"invocable-execution-v0\",\"operation_name\":\"external.calculator\",\"probe_arguments\":{{\"operation\":\"multiply\",\"a\":6,\"b\":7}}}}}}");
        return;
    }
    let Some(a) = number_field(&input, "a") else {
        respond_error("invalid_arguments");
        return;
    };
    let Some(b) = number_field(&input, "b") else {
        respond_error("invalid_arguments");
        return;
    };
    match operation.as_str() {
        "add" => respond_number(a + b),
        "subtract" => respond_number(a - b),
        "multiply" => respond_number(a * b),
        "divide" if b == 0.0 => respond_error("divide_by_zero"),
        "divide" => respond_number(a / b),
        _ => respond_error("unsupported_operation"),
    }
}

fn string_field(input: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\"");
    let tail = input.get(input.find(&marker)? + marker.len()..)?;
    let tail = tail.get(tail.find(':')? + 1..)?.trim_start();
    let tail = tail.strip_prefix('"')?;
    Some(tail.get(..tail.find('"')?)?.to_string())
}

fn number_field(input: &str, key: &str) -> Option<f64> {
    let marker = format!("\"{key}\"");
    let tail = input.get(input.find(&marker)? + marker.len()..)?;
    let tail = tail.get(tail.find(':')? + 1..)?.trim_start();
    let end = tail.find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')))
        .unwrap_or(tail.len());
    tail.get(..end)?.parse().ok()
}

fn respond_number(value: f64) {
    if value.is_finite() && value.fract() == 0.0 {
        let _ = writeln!(std::io::stdout(), "{{\"ok\":true,\"result\":{}}}", value as i64);
    } else {
        let _ = writeln!(std::io::stdout(), "{{\"ok\":true,\"result\":{value}}}");
    }
}

fn respond_error(code: &str) {
    let _ = writeln!(std::io::stdout(), "{{\"ok\":false,\"error\":{{\"code\":\"{code}\"}}}}");
}
"#;

pub fn generate(
    artifact_root: &Path,
    request: &DevelopmentRequest,
) -> Result<Value, std::io::Error> {
    if !supports(request) {
        return Err(std::io::Error::other("calculator fixture mismatch"));
    }
    generate_locked(artifact_root, &request.idempotency_key, &request.request_id)
}

pub(super) fn supports(request: &DevelopmentRequest) -> bool {
    request.target_kind == TargetKind::InvocableCapability
        && request.name == "external.calculator"
        && request.build_profile == "invocable-capability-v0"
        && request.required_contracts == ["component.invoke.v0"]
}

fn generate_locked(
    artifact_root: &Path,
    key: &str,
    request_id: &str,
) -> Result<Value, std::io::Error> {
    let key_hash = hex::encode(Sha256::digest(key.as_bytes()));
    let candidate_id = format!("calculator_{}", &key_hash[..24]);
    let base = artifact_root.join("generated");
    std::fs::create_dir_all(&base)?;
    let lock_path = base.join(format!("{candidate_id}.lock"));
    let mut lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;

    let candidate = base.join(&candidate_id).join("candidate");
    if !candidate.is_dir() {
        let temp = base.join(format!(".{candidate_id}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(temp.join("candidate/src"))?;
        std::fs::write(temp.join("candidate/Cargo.toml"), CARGO_TOML)?;
        std::fs::write(temp.join("candidate/manifest.json"), CANDIDATE_MANIFEST)?;
        std::fs::write(temp.join("candidate/src/main.rs"), MAIN_RS)?;
        std::fs::rename(temp, base.join(&candidate_id))?;
    }
    let digest =
        crate::hcr::candidate::compute_digest(&candidate).map_err(std::io::Error::other)?;
    let component_manifest: Value =
        serde_json::from_str(CANDIDATE_MANIFEST).map_err(std::io::Error::other)?;
    writeln!(lock, "{digest}")?;
    let _ = lock.unlock();
    Ok(json!({
        "candidate_id": candidate_id,
        "candidate_ref": format!("generated/{candidate_id}/candidate"),
        "candidate_digest": digest,
        "request_id": request_id,
        "component_manifest": component_manifest,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
    use agent_core_kernel::domain::DevelopmentRequestDraft;

    fn calculator_request() -> DevelopmentRequest {
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.calculator".into(),
        );
        draft.requirements = vec!["four arithmetic operations".into()];
        draft.required_contracts = vec!["component.invoke.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["6 * 7 = 42".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "scope:test".into(),
            "message:test".into(),
            "calculator:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    #[test]
    fn fixture_materializes_generic_component_manifest() {
        let root = std::env::temp_dir().join(format!(
            "calculator_fixture_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let result = generate(&root, &calculator_request()).unwrap();
        assert_eq!(
            result["component_manifest"]["profile_id"],
            "invocable-capability-v0"
        );
        assert_eq!(
            result["component_manifest"]["test_kit"],
            "calculator-fixture-v0"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
