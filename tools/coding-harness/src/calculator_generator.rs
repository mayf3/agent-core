//! Fixed `calculator-v0` candidate generator.
//!
//! This is deliberately not a general code-generation endpoint.  It accepts
//! exactly the North Star calculator specification and materialises a fresh,
//! deterministic candidate workspace below the Harness artifact root.

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
  "manifest_id": "calculator-v0-candidate",
  "harness_id": "calculator-harness",
  "protocol_version": "external-harness-v1",
  "operation": "external.calculator",
  "operations": ["add", "subtract", "multiply", "divide"],
  "description": "Calculator supporting add, subtract, multiply, and divide.",
  "entry": "target/release/calculator-harness",
  "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
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

/// Generate or replay the single allowed calculator candidate.
pub fn handle_submit(artifact_root: &Path, args: &Value) -> Value {
    if let Err(code) = validate_spec(args) {
        return error(code);
    }
    let idempotency_key = args
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    if idempotency_key.is_empty() {
        return error("MISSING_IDEMPOTENCY_KEY");
    }

    match generate_locked(artifact_root, idempotency_key) {
        Ok(result) => json!({
            "protocol_version": "external-harness-v1",
            "ok": true,
            "result": result,
        }),
        Err(_) => error("CANDIDATE_GENERATION_FAILED"),
    }
}

fn validate_spec(args: &Value) -> Result<(), &'static str> {
    let exact = |key: &str, expected: &str| args.get(key).and_then(Value::as_str) == Some(expected);
    if !exact("kind", "DevelopCapability")
        || !exact("operation", "external.calculator")
        || !exact("schema_version", "calculator-v0")
    {
        return Err("UNSUPPORTED_CODING_SPEC");
    }
    let functions = args
        .get("functions")
        .and_then(Value::as_array)
        .ok_or("INVALID_FUNCTION_SET")?;
    let actual: Vec<&str> = functions.iter().filter_map(Value::as_str).collect();
    if actual != ["add", "subtract", "multiply", "divide"] {
        return Err("INVALID_FUNCTION_SET");
    }
    Ok(())
}

fn generate_locked(artifact_root: &Path, key: &str) -> Result<Value, std::io::Error> {
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
    writeln!(lock, "{digest}")?;
    let _ = lock.unlock();
    Ok(json!({
        "candidate_id": candidate_id,
        "candidate_ref": format!("generated/{candidate_id}/candidate"),
        "candidate_digest": digest,
        "operation": "external.calculator",
        "schema_version": "calculator-v0",
    }))
}

fn error(code: &str) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_spec_creates_fresh_candidate_and_replays() {
        let root = std::env::temp_dir().join(format!(
            "calculator_generator_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let args = json!({
            "kind": "DevelopCapability",
            "operation": "external.calculator",
            "functions": ["add", "subtract", "multiply", "divide"],
            "schema_version": "calculator-v0",
            "idempotency_key": "message-1",
        });
        let first = handle_submit(&root, &args);
        let second = handle_submit(&root, &args);
        assert_eq!(first["result"], second["result"]);
        let rel = first["result"]["candidate_ref"].as_str().unwrap();
        assert!(root.join(rel).join("src/main.rs").is_file());
    }

    #[test]
    fn arbitrary_spec_is_rejected() {
        let response = handle_submit(
            Path::new("/tmp"),
            &json!({"operation":"external.shell","idempotency_key":"x"}),
        );
        assert_eq!(response["ok"], false);
    }
}
