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
"description": "写入模式：replace 覆盖，append 追加。",
                    }
                },
                "required": ["workspace_id", "relative_path", "content"],
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
    ]
}

/// Build a complete `HarnessManifest` for each canonical spec.
pub fn build_manifests(
    workspace_ids: &[String],
    endpoint: &str,
    artifact_digest: &str,
) -> Vec<agent_core_kernel::harness::manifest::HarnessManifest> {
    let specs = all_specs(workspace_ids);
    specs.iter().map(|spec| {
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
    }).collect()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_coding_operations_have_nontrivial_input_schema() {
        let specs = all_specs(&["agent-dev".to_string()]);
        assert_eq!(specs.len(), 7);
        for spec in &specs {
            let obj = spec.input_schema.as_object().unwrap();
            assert!(obj.get("properties").and_then(|p| p.as_object()).is_some_and(|p| !p.is_empty()),
                "{}: properties must be non-empty", spec.operation_name);
            assert!(!spec.description.is_empty(), "{}: description must be non-empty", spec.operation_name);
            let required: Vec<&str> = obj.get("required").unwrap().as_array().unwrap().iter()
                .filter_map(|v| v.as_str()).collect();
            assert!(!required.is_empty(), "{}: required must be non-empty", spec.operation_name);
            if spec.operation_name != "external.coding_task_status" {
                assert!(required.contains(&"workspace_id"), "{}: must require workspace_id", spec.operation_name);
                let ws = spec.input_schema.pointer("/properties/workspace_id").unwrap();
                let ev = ws.get("enum").and_then(|e| e.as_array()).unwrap();
                assert!(ev.contains(&json!("agent-dev")), "{}: enum must contain agent-dev", spec.operation_name);
            }
        }
    }

    #[test]
    fn task_status_does_not_require_workspace_id() {
        let specs = all_specs(&["agent-dev".to_string()]);
        let ts = specs.iter().find(|s| s.operation_name == "external.coding_task_status").unwrap();
        let required: Vec<&str> = ts.input_schema.get("required").unwrap().as_array().unwrap().iter()
            .filter_map(|v| v.as_str()).collect();
        assert!(!required.contains(&"workspace_id"));
        assert!(required.contains(&"task_id"));
    }

    #[test]
    fn capability_propose_schema_is_complete() {
        let specs = all_specs(&["agent-dev".to_string()]);
        let cp = specs.iter().find(|s| s.operation_name == "external.coding_capability_propose").unwrap();
        let required: Vec<&str> = cp.input_schema.get("required").unwrap().as_array().unwrap().iter()
            .filter_map(|v| v.as_str()).collect();
        assert!(required.contains(&"workspace_id"));
        assert!(required.contains(&"artifact_path"));
        assert!(required.contains(&"manifest_path"));
        assert!(required.contains(&"evidence_path"));
    }

    #[test]
    fn workspace_id_enum_contains_multiple_ids() {
        let ids = vec!["agent-dev".to_string(), "prod".to_string()];
        let specs = all_specs(&ids);
        let ws_prop = specs[0].input_schema.pointer("/properties/workspace_id").unwrap();
        let desc = ws_prop.get("description").and_then(|d| d.as_str()).unwrap();
        assert!(desc.contains("agent-dev") && desc.contains("prod"), "desc: {desc}");
    }

    #[test]
    fn no_operation_uses_generic_object_schema() {
        for spec in &all_specs(&["test".to_string()]) {
            assert_ne!(serde_json::to_string(&spec.input_schema).unwrap(), r#"{"type":"object"}"#,
                "{}: must not use generic object schema", spec.operation_name);
        }
    }

    #[test]
    fn coding_manifests_are_deterministic_and_unique() {
        let ws = vec!["agent-dev".to_string()];
        let ep = "http://127.0.0.1:7200";
        let ad = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let m1 = build_manifests(&ws, ep, ad);
        let m2 = build_manifests(&ws, ep, ad);
        assert_eq!(m1.len(), 7);
        for i in 0..7 {
            assert_eq!(m1[i].manifest_id, m2[i].manifest_id,
                "deterministic: {}", m1[i].operation_name);
            let recomputed = m1[i].compute_manifest_id().unwrap();
            assert_eq!(recomputed, m1[i].manifest_id,
                "compute_manifest_id matches stored: {}", m1[i].operation_name);
        }
        let mut names: Vec<&str> = m1.iter().map(|m| m.operation_name.as_str()).collect();
        names.sort(); names.dedup();
        assert_eq!(names.len(), 7);
        let mut mids: Vec<&str> = m1.iter().map(|m| m.manifest_id.as_str()).collect();
        mids.sort(); mids.dedup();
        assert_eq!(mids.len(), 7);
        for m in &m1 {
            let s = serde_json::to_string(&m.input_schema).unwrap();
            assert!(!s.contains("/tmp/"), "no workspace root in {}", m.operation_name);
            assert!(!s.contains("sk-"), "no secret in {}", m.operation_name);
        }
        eprintln!("=== Deterministic manifest IDs (workspace_ids=[agent-dev], endpoint={ep}) ===");
        for m in &m1 { eprintln!("  {}: {}", m.operation_name, m.manifest_id); }
    }

    #[test]
    fn coding_manifest_registration_chain_preserves_schema() {
        use agent_core_kernel::gateway::Gateway;
        use agent_core_kernel::harness::control::{HarnessChangeAction, HarnessChangeIntent};
        use agent_core_kernel::journal::JournalStore;

        let j = JournalStore::in_memory().unwrap();
        let g = Gateway::new(agent_core_kernel::config::KernelConfig {
            db_path: std::path::PathBuf::from(":memory:"), data_dir: std::path::PathBuf::from("."),
            agent_id: agent_core_kernel::domain::AgentId("main".into()), root_dir: std::path::PathBuf::from("."),
            kernel_port: 4130, connector_execute_url: String::new(), ipc_token: "test".into(),
            capability_submit_token: None, capability_decision_token: None,
            feishu_allowed_open_ids: vec![], feishu_allowed_chat_ids: vec![],
            feishu_require_group_mention: true, openai_base_url: String::new(), openai_api_key: String::new(),
            model: String::new(), fallback_openai_base_url: String::new(),
            fallback_openai_api_key: String::new(), fallback_model: String::new(),
            model_timeout_ms: 5000, context_recent_messages: 10, context_max_block_chars: 10000,
            outbox_dispatcher_enabled: false, outbox_dispatcher_poll_interval_ms: 1000,
            extra_allowed_operations: vec![], require_write_approval: false, write_approval_ttl_secs: 0,
            fallback_tool_name_indexed: false, primary_tool_name_indexed: false,
            harness_read_timeout_ms: 10000, harness_artifact_root: std::path::PathBuf::from("."),
            max_tool_rounds: 12,
        });
        let manifests = build_manifests(&vec!["agent-dev".to_string(), "prod".to_string()],
            "http://127.0.0.1:7200", "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        for mut m in manifests {
            let mid = m.compute_manifest_id().unwrap();
            m.manifest_id = mid.clone();
            j.register_harness_manifest(&m).unwrap();
            j.enable_harness(&g.approve_harness_change(HarnessChangeIntent {
                action: HarnessChangeAction::Enable, manifest_id: mid.clone(),
                expected_snapshot_id: j.current_registry_snapshot_id().unwrap(),
                requested_by: "ipc_operator".into(),
            }).unwrap()).unwrap();
        }
        let snap_id = j.current_registry_snapshot_id().unwrap();
        let snap = j.load_registry_snapshot(&snap_id).unwrap();
        let all_ops: Vec<String> = vec![
            "external.coding_workspace_list".into(), "external.coding_workspace_read".into(),
            "external.coding_workspace_write".into(), "external.coding_workspace_exec".into(),
            "external.coding_task_submit".into(), "external.coding_task_status".into(),
            "external.coding_capability_propose".into(),
        ];
        let tools = snap.provider_tools_for_grants(&all_ops);
        assert_eq!(tools.len(), 7);

        let cp = tools.iter().find(|t| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) == Some("external.coding_capability_propose")).unwrap();
        let func = cp.get("function").unwrap();
        assert!(!func.get("description").and_then(|v| v.as_str()).unwrap_or("").is_empty());
        let params = func.get("parameters").unwrap();
        let required: Vec<&str> = params.get("required").unwrap().as_array().unwrap().iter()
            .filter_map(|v| v.as_str()).collect();
        assert!(required.contains(&"workspace_id") && required.contains(&"artifact_path")
            && required.contains(&"manifest_path") && required.contains(&"evidence_path"));
        let ws_prop = params.pointer("/properties/workspace_id").unwrap();
        let ev = ws_prop.get("enum").and_then(|v| v.as_array()).unwrap();
        assert!(ev.contains(&serde_json::json!("agent-dev")) && ev.contains(&serde_json::json!("prod")));

        let ts = tools.iter().find(|t| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) == Some("external.coding_task_status")).unwrap();
        let ts_req: Vec<&str> = ts.get("function").unwrap().get("parameters").unwrap().get("required").unwrap().as_array().unwrap().iter()
            .filter_map(|v| v.as_str()).collect();
        assert!(ts_req.contains(&"task_id") && !ts_req.contains(&"workspace_id"));

        for tool in &tools {
            assert!(!tool.get("function").unwrap().get("description").and_then(|v| v.as_str()).unwrap_or("").is_empty());
        }
        assert!(j.verify_hash_chain().unwrap());
    }
}
