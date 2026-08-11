//! Canonical model-facing schemas and HarnessManifest builder.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const LIST: &str = "external.execution_list";
pub const READ: &str = "external.execution_read";
pub const WRITE: &str = "external.execution_write";
pub const EXEC: &str = "external.execution_exec";

pub struct OperationSpec {
    pub operation_name: &'static str,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub idempotent: bool,
}

fn path_description(allowed_roots: &[String], subject: &str) -> String {
    format!(
        "Absolute {subject} path. It must resolve inside one of these authoritative source roots: {}. Deployment copies are not source.",
        allowed_roots.join(", ")
    )
}

pub fn all_specs(allowed_roots: &[String]) -> Vec<OperationSpec> {
    let path = path_description(allowed_roots, "filesystem");
    let cwd = path_description(allowed_roots, "working-directory");
    vec![
        OperationSpec {
            operation_name: LIST,
            description: "List one directory by its real absolute path.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": path.clone()}},
                "required": ["path"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "entries": {"type": "array", "items": {"type": "object"}},
                    "entry_count": {"type": "integer"}
                },
                "required": ["path", "entries", "entry_count"]
            }),
            idempotent: true,
        },
        OperationSpec {
            operation_name: READ,
            description: "Read one UTF-8 file by its real absolute path.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": path.clone()},
                    "max_bytes": {"type": "integer", "minimum": 1, "maximum": 65536}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                    "size_bytes": {"type": "integer"},
                    "bytes_read": {"type": "integer"},
                    "truncated": {"type": "boolean"}
                },
                "required": ["path", "content", "size_bytes", "bytes_read", "truncated"]
            }),
            idempotent: true,
        },
        OperationSpec {
            operation_name: WRITE,
            description: "Replace one file by its real absolute path. The parent directory must already exist.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": path},
                    "content": {"type": "string", "description": "Complete replacement content."}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "bytes_written": {"type": "integer"}
                },
                "required": ["path", "bytes_written"]
            }),
            idempotent: true,
        },
        OperationSpec {
            operation_name: EXEC,
            description: "Run one complete shell command in an absolute cwd. Pipes, redirects, quotes and && are supported inside the sandbox.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": {"type": "string", "description": cwd},
                    "command": {"type": "string", "description": "Complete POSIX shell command, for example: git status --short or rg 'RunFailed' src | head."}
                },
                "required": ["cwd", "command"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": {"type": "string"},
                    "exit_code": {"type": "integer"},
                    "stdout": {"type": "string"},
                    "stderr": {"type": "string"},
                    "timed_out": {"type": "boolean"},
                    "stdout_bytes": {"type": "integer"},
                    "stderr_bytes": {"type": "integer"},
                    "stdout_truncated": {"type": "boolean"},
                    "stderr_truncated": {"type": "boolean"},
                    "sandboxed": {"type": "boolean"}
                },
                "required": ["cwd", "exit_code", "stdout", "stderr", "timed_out", "stdout_bytes", "stderr_bytes", "stdout_truncated", "stderr_truncated", "sandboxed"]
            }),
            idempotent: false,
        },
    ]
}

#[derive(Serialize)]
pub struct Manifest {
    pub manifest_id: String,
    pub harness_id: String,
    pub artifact_digest: String,
    pub protocol_version: String,
    pub endpoint: String,
    pub operation_name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub idempotent: bool,
    pub created_at: DateTime<Utc>,
}

impl Manifest {
    fn compute_id(&self) -> String {
        let canonical = json!({
            "harness_id": self.harness_id,
            "artifact_digest": self.artifact_digest,
            "protocol_version": self.protocol_version,
            "endpoint": self.endpoint,
            "operation_name": self.operation_name,
            "description": self.description,
            "input_schema": self.input_schema,
            "output_schema": self.output_schema,
            "idempotent": self.idempotent,
        });
        let encoded = serde_json::to_vec(&canonical).expect("canonical manifest JSON");
        format!("manifest_{}", hex::encode(Sha256::digest(encoded)))
    }
}

pub fn build_manifests(
    allowed_roots: &[String],
    endpoint: &str,
    artifact_digest: &str,
) -> Vec<Manifest> {
    all_specs(allowed_roots)
        .into_iter()
        .map(|spec| {
            let mut manifest = Manifest {
                manifest_id: String::new(),
                harness_id: "execution-harness-v0".into(),
                artifact_digest: artifact_digest.into(),
                protocol_version: "external-harness-v1".into(),
                endpoint: endpoint.into(),
                operation_name: spec.operation_name.into(),
                description: spec.description,
                input_schema: spec.input_schema,
                output_schema: spec.output_schema,
                idempotent: spec.idempotent,
                created_at: Utc::now(),
            };
            manifest.manifest_id = manifest.compute_id();
            manifest
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_schemas_use_only_paths_and_one_shell_command() {
        let root = "/source/agent-core".to_string();
        let specs = all_specs(std::slice::from_ref(&root));
        assert_eq!(specs.len(), 4);
        let encoded = serde_json::to_string(
            &specs
                .iter()
                .map(|spec| &spec.input_schema)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(!encoded.contains("workspace_id"));
        assert!(!encoded.contains("available_workspace_ids"));
        assert!(!encoded.contains("relative_cwd"));
        assert!(!encoded.contains("\"args\""));
        assert!(!encoded.contains("\"program\""));
        assert!(!encoded.contains("timeout_seconds"));
        assert!(!encoded.contains("max_output_bytes"));
        let exec = specs
            .iter()
            .find(|spec| spec.operation_name == EXEC)
            .unwrap();
        assert_eq!(exec.input_schema["required"], json!(["cwd", "command"]));
        assert!(encoded.contains(&root));
    }

    #[test]
    fn manifest_ids_are_stable_across_created_at() {
        let roots = vec!["/source/agent-core".into()];
        let first = build_manifests(
            &roots,
            "http://127.0.0.1:7650/execute",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        );
        let second = build_manifests(
            &roots,
            "http://127.0.0.1:7650/execute",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert_eq!(first[0].manifest_id, second[0].manifest_id);
    }
}
