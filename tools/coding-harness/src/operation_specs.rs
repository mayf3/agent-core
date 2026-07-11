//! Canonical operation specifications for the Coding Harness.
//!
//! Every `external.coding_*` operation has a single source of truth for its
//! name, description, input schema, and output schema. Production registration,
//! test fixtures, and LLM tool definitions all derive from these specs.
//!
//! Workspace IDs are injected at registration time: the `workspace_id` field
//! in each schema's `properties` includes an `enum` constrained to the IDs the
//! Coding Harness knows about. The hostname / absolute path is never exposed.

use serde_json::{json, Value};

/// A single operation specification.
pub struct OperationSpec {
    pub operation_name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub output_schema: Value,
}

/// Build standard workspace_id property with enum from available IDs.
fn workspace_id_property(workspace_ids: &[String]) -> Value {
    let desc = if workspace_ids.len() == 1 {
        format!(
            "授权 workspace 的 ID。当前可用 workspace: {}",
            workspace_ids[0]
        )
    } else if workspace_ids.len() > 1 {
        format!(
            "授权 workspace 的 ID。当前可用 workspaces: {}",
            workspace_ids.join(", ")
        )
    } else {
        "授权 workspace 的 ID。".to_string()
    };
    let ids: Vec<Value> = workspace_ids.iter().map(|id| json!(id)).collect();
    json!({
        "type": "string",
        "description": desc,
        "enum": ids,
    })
}

/// Return the canonical specs for all seven Coding Harness operations, with
/// `workspace_id` description containing the authorized workspace IDs.
pub fn all_specs(workspace_ids: &[String]) -> Vec<OperationSpec> {
    let ws = workspace_id_property(workspace_ids);

    vec![
        OperationSpec {
            operation_name: "external.coding_workspace_list",
            description: "列出授权 workspace 中的文件和目录。返回名称、类型（文件/目录）和相对路径。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": ws,
                    "relative_path": {
                        "type": "string",
                        "description": "相对于 workspace root 的目录路径。",
                    }
                },
                "required": ["workspace_id"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "entries": {
                        "type": "array",
                        "items": {"type": "object"}
                    }
                },
                "required": ["entries"],
            }),
        },
        OperationSpec {
            operation_name: "external.coding_workspace_read",
            description: "读取授权 workspace 中的文件内容。返回内容及截断标志。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": ws,
                    "relative_path": {
                        "type": "string",
                        "description": "相对于 workspace root 的文件路径。"
                    },
                    "max_bytes": {
                        "type": "integer",
                        "description": "最大读取字节数，不超过 65536。",
                        "maximum": 65536
                    }
                },
                "required": ["workspace_id", "relative_path"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string"},
                    "truncated": {"type": "boolean"},
                    "size_bytes": {"type": "integer"},
                    "bytes_read": {"type": "integer"}
                },
                "required": ["content", "truncated", "size_bytes", "bytes_read"],
            }),
        },
        OperationSpec {
            operation_name: "external.coding_workspace_write",
            description: "向授权 workspace 写入文件。支持替换或追加模式。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": ws,
                    "relative_path": {
                        "type": "string",
                        "description": "相对于 workspace root 的文件路径。"
                    },
                    "content": {
                        "type": "string",
                        "description": "写入的文件内容。"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["replace", "append"],
"description": "写入模式：replace 覆盖，append 追加。",
                    }
                },
                "required": ["workspace_id", "relative_path", "content"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "bytes_written": {"type": "integer"},
                    "sha256": {"type": "string"},
                    "mode": {"type": "string"}
                },
                "required": ["bytes_written", "sha256", "mode"],
            }),
        },
        OperationSpec {
            operation_name: "external.coding_workspace_exec",
            description: "在授权 workspace 中执行命令。默认直接通过 argv 执行，不经过 shell。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": ws,
                    "command": {
                        "type": "string",
                        "description": "要执行的命令名称（例如 rustc、cargo、python3）。不经过 shell。"
                    },
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "命令参数数组。",
                    },
                    "relative_cwd": {
                        "type": "string",
                        "description": "相对于 workspace root 的工作目录。",
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "超时秒数，最长 3600。",
                        "maximum": 3600
                    },
                    "max_output_bytes": {
                        "type": "integer",
                        "description": "输出最大字节数，最长 1048576。",
                        "maximum": 1048576
                    },
                    "shell": {
                        "type": "boolean",
                        "description": "设置为 true 时通过 shell 执行命令。默认 false 表示直接 argv 执行。",
                    }
                },
                "required": ["workspace_id", "command"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "exit_code": {"type": "integer"},
                    "stdout": {"type": "string"},
                    "stderr": {"type": "string"},
                    "timed_out": {"type": "boolean"}
                },
                "required": ["exit_code", "stdout", "stderr", "timed_out"],
            }),
        },
        OperationSpec {
            operation_name: "external.coding_task_submit",
            description: "提交一个编码任务到后台执行（opencode 后端）。提交后使用 external.coding_task_status 查询进度和结果。这是长时间运行的任务，不会立即返回完成状态。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": ws,
                    "backend": {
                        "type": "string",
                        "enum": ["opencode"],
                        "description": "任务后端。生产环境请使用 opencode。",
                    },
                    "objective": {
                        "type": "string",
                        "description": "任务的详细目标和说明。"
                    },
                    "acceptance_criteria": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "验收标准列表。任务完成后将逐项检查。",
                    },
                    "model": {
                        "type": "string",
                        "description": "模型标识，格式为 provider/model。",
                    }
                },
                "required": ["workspace_id", "backend", "objective"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string"},
                    "status": {"type": "string"},
                    "backend": {"type": "string"},
                    "created_at": {"type": "integer"}
                },
                "required": ["task_id", "status", "backend", "created_at"],
            }),
        },
        OperationSpec {
            operation_name: "external.coding_task_status",
            description: "查询 external.coding_task_submit 提交的任务当前状态和结果。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "coding_task_submit 返回的任务 ID。"
                    }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string"},
                    "status": {"type": "string"},
                    "summary": {"type": "string"},
                    "changed_files": {"type": "string"},
                    "test_result": {"type": "string"}
                },
                "required": ["task_id", "status"],
            }),
        },
        OperationSpec {
            operation_name: "external.coding_capability_propose",
            description: "提交 Capability Proposal，将外部工具注册到 Kernel。只有在 artifact、manifest 和 evidence 已存在且测试通过后才能调用。该操作只提交 PendingApproval Proposal，不会自动批准。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": ws,
                    "artifact_path": {
                        "type": "string",
                        "description": "相对于授权 workspace root 的 artifact 文件路径。不接受绝对路径或 ..。文件必须已由前序构建步骤生成。"
                    },
                    "manifest_path": {
                        "type": "string",
                        "description": "相对于授权 workspace root 的 manifest JSON 文件路径。不接受绝对路径或 ..。文件必须已存在。"
                    },
                    "evidence_path": {
                        "type": "string",
                        "description": "相对于授权 workspace root 的 evidence 文件路径。不接受绝对路径或 ..。文件必须已存在。"
                    }
                },
                "required": ["workspace_id", "artifact_path", "manifest_path", "evidence_path"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "proposal_id": {"type": "string"},
                    "status": {"type": "string"},
                    "artifact_digest": {"type": "string"},
                    "manifest_digest": {"type": "string"},
                    "evidence_digest": {"type": "string"},
                    "manifest_id": {"type": "string"},
                    "operation_name": {"type": "string"},
                    "expected_active_snapshot_id": {"type": "string"},
                    "expires_at": {"type": "string"}
                },
                "required": ["proposal_id", "status"],
            }),
        },
        OperationSpec {
            operation_name: "external.coding_hcr_exec",
            description: "在 HCR 安全执行 profile 下执行受控命令。不使用 shell，命令由 HCR profile 定义。需要有效的 HCR token。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": ws,
                    "hcr_profile_id": {
                        "type": "string",
                        "description": "HCR 执行 profile 的 ID（例如 hcr-v0）。profile 必须已在 CODING_CONFIG 中配置。"
                    },
                    "hcr_token": {
                        "type": "string",
                        "description": "HCR 认证 token，用于验证调用方有权使用 HCR profile。"
                    },
                    "command": {
                        "type": "string",
                        "description": "HCR profile 中定义的命令名称（例如 node_test、harness_local_smoke）。"
                    },
                    "params": {
                        "type": "object",
                        "description": "命令参数，key-value 键值对。键对应于命令模板中的参数名。",
                        "additionalProperties": true
                    }
                },
                "required": ["workspace_id", "hcr_profile_id", "hcr_token", "command"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "status": {"type": "string"},
                    "exit_code": {"type": "integer"},
                    "timed_out": {"type": "boolean"},
                    "stdout": {"type": "string"},
                    "stderr": {"type": "string"},
                    "stdout_truncated": {"type": "boolean"},
                    "stderr_truncated": {"type": "boolean"},
                    "child_cleanup": {"type": "string"},
                    "error_code": {"type": "string"}
                },
                "required": ["status", "exit_code", "timed_out", "child_cleanup"],
            }),
        },
    ]
}

/// Build a complete `HarnessManifest` for each canonical spec.
pub fn build_manifests(
    workspace_ids: &[String],
    endpoint: &str,
    artifact_digest: &str,
) -> Vec<agent_core_kernel::harness::manifest::HarnessManifest> {
    let specs = all_specs(workspace_ids);
    specs
        .iter()
        .map(|spec| {
            let mut m = agent_core_kernel::harness::manifest::HarnessManifest {
                manifest_id: String::new(),
                harness_id: "coding-harness-v0".to_string(),
                artifact_digest: artifact_digest.to_string(),
                protocol_version: "external-harness-v1".to_string(),
                endpoint: endpoint.to_string(),
                operation_name: spec.operation_name.to_string(),
                description: spec.description.to_string(),
                input_schema: spec.input_schema.clone(),
                output_schema: spec.output_schema.clone(),
                idempotent: true,
                created_at: chrono::Utc::now(),
            };
            if let Ok(id) = m.compute_manifest_id() {
                m.manifest_id = id;
            }
            m
        })
        .collect()
}
