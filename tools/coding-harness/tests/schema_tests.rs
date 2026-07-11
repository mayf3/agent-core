use coding_harness::operation_specs;
#[test]
fn coding_manifest_registration_chain_preserves_schema() {
    use agent_core_kernel::harness::control::{
        ApprovedHarnessChange, HarnessChangeAction, HarnessChangeIntent,
    };
    use agent_core_kernel::journal::JournalStore;
    let specs = operation_specs::all_specs(&["agent-dev".to_string()]);
    for spec in &specs {
        let obj = spec.input_schema.as_object().unwrap();
        assert_eq!(obj.get("type").and_then(|v| v.as_str()), Some("object"));
        assert!(obj
            .get("properties")
            .and_then(|p| p.as_object())
            .is_some_and(|p| !p.is_empty()));
        assert!(obj.get("additionalProperties").and_then(|v| v.as_bool()) == Some(false));
        assert!(!spec.description.is_empty());
        let req: Vec<&str> = obj
            .get("required")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(!req.is_empty());
        if spec.operation_name == "external.coding_task_status" {
            assert!(req.contains(&"task_id") && !req.contains(&"workspace_id"));
        } else {
            assert!(req.contains(&"workspace_id"));
            let ev = spec
                .input_schema
                .pointer("/properties/workspace_id")
                .unwrap()
                .get("enum")
                .unwrap()
                .as_array()
                .unwrap();
            assert!(ev.contains(&serde_json::json!("agent-dev")));
        }
    }
    let write = specs
        .iter()
        .find(|s| s.operation_name == "external.coding_workspace_write")
        .unwrap();
    let mode_ev = write
        .input_schema
        .pointer("/properties/mode")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        mode_ev
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>(),
        vec!["replace", "append"]
    );
    let submit = specs
        .iter()
        .find(|s| s.operation_name == "external.coding_task_submit")
        .unwrap();
    let be_ev = submit
        .input_schema
        .pointer("/properties/backend")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        be_ev.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
        vec!["opencode"]
    );
    let j = JournalStore::in_memory().unwrap();
    for mut m in operation_specs::build_manifests(
        &vec!["agent-dev".to_string(), "prod".to_string()],
        "http://127.0.0.1:7200",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ) {
        let mid = m.compute_manifest_id().unwrap();
        m.manifest_id = mid.clone();
        j.register_harness_manifest(&m).unwrap();
        j.enable_harness(&ApprovedHarnessChange {
            intent: HarnessChangeIntent {
                action: HarnessChangeAction::Enable,
                manifest_id: mid.clone(),
                expected_snapshot_id: j.current_registry_snapshot_id().unwrap(),
                requested_by: "ipc_operator".into(),
            },
            decision_id: "decision_test".into(),
        })
        .unwrap();
    }
    let snap = j
        .load_registry_snapshot(&j.current_registry_snapshot_id().unwrap())
        .unwrap();
    let tools = snap.provider_tools_for_grants(
        &[
            "external.coding_workspace_list",
            "external.coding_workspace_read",
            "external.coding_workspace_write",
            "external.coding_workspace_exec",
            "external.coding_task_submit",
            "external.coding_task_status",
            "external.coding_capability_propose",
        ]
        .map(String::from),
    );
    assert_eq!(tools.len(), 7);
    let fn_tool = |name: &str| {
        tools
            .iter()
            .find(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    == Some(name)
            })
            .unwrap()
    };
    let cp_params = fn_tool("external.coding_capability_propose")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert_eq!(
        cp_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let cp_req: Vec<&str> = cp_params
        .get("required")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        cp_req.contains(&"workspace_id")
            && cp_req.contains(&"artifact_path")
            && cp_req.contains(&"manifest_path")
            && cp_req.contains(&"evidence_path")
    );
    let cp_ev = cp_params
        .pointer("/properties/workspace_id")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(
        cp_ev.contains(&serde_json::json!("agent-dev"))
            && cp_ev.contains(&serde_json::json!("prod"))
    );
    let write_params = fn_tool("external.coding_workspace_write")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert_eq!(
        write_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let wmode = write_params
        .pointer("/properties/mode")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        wmode.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
        vec!["replace", "append"]
    );
    let submit_params = fn_tool("external.coding_task_submit")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert_eq!(
        submit_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let sbe = submit_params
        .pointer("/properties/backend")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        sbe.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
        vec!["opencode"]
    );
    let ts_params = fn_tool("external.coding_task_status")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert_eq!(
        ts_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let ts_req: Vec<&str> = ts_params
        .get("required")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(ts_req.contains(&"task_id") && !ts_req.contains(&"workspace_id"));
    assert!(j.verify_hash_chain().unwrap());
}
#[test]
fn coding_manifest_llm_input_receives_complete_tool_definitions() {
    use agent_core_kernel::gateway::Gateway;
    use agent_core_kernel::harness::control::{
        ApprovedHarnessChange, HarnessChangeAction, HarnessChangeIntent,
    };
    use agent_core_kernel::journal::JournalStore;
    use agent_core_kernel::llm::LlmClient;
    use agent_core_kernel::llm::{LlmInput, LlmOutput};
    use agent_core_kernel::runtime::Runtime;
    use serde_json::Value;
    use std::sync::Arc;
    use std::sync::Mutex;
    struct CaptureLlm {
        captured: Arc<Mutex<Vec<Value>>>,
    }
    impl LlmClient for CaptureLlm {
        fn complete(&self, input: LlmInput) -> anyhow::Result<LlmOutput> {
            let mut c = self.captured.lock().unwrap();
            if c.is_empty() {
                *c = input.provider_tools.clone();
            }
            drop(c);
            Ok(LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "ok".into(),
                journal_payload: serde_json::json!({"r":0}),
                tool_call: agent_core_kernel::llm::ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
    let config = agent_core_kernel::config::KernelConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        data_dir: std::path::PathBuf::from("."),
        agent_id: agent_core_kernel::domain::AgentId("main".into()),
        root_dir: std::path::PathBuf::from("."),
        kernel_port: 4130,
        connector_execute_url: String::new(),
        ipc_token: "test".into(),
        capability_submit_token: None,
        capability_decision_token: None,
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_coding_owner_id: Some("owner".into()),
        feishu_require_group_mention: true,
        openai_base_url: String::new(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 5000,
        context_recent_messages: 10,
        context_max_block_chars: 10000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 1000,
        extra_allowed_operations: vec![
            "external.coding_workspace_list".to_string(),
            "external.coding_workspace_read".to_string(),
            "external.coding_workspace_write".to_string(),
            "external.coding_workspace_exec".to_string(),
            "external.coding_task_submit".to_string(),
            "external.coding_task_status".to_string(),
            "external.coding_capability_propose".to_string(),
        ],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10000,
        harness_artifact_root: std::path::PathBuf::from("."),
        max_tool_rounds: 12,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: Default::default(),
    };
    let j = JournalStore::in_memory().unwrap();
    let g = Gateway::new(config.clone());
    let store = Arc::new(Mutex::new(Vec::new()));
    let runtime = Runtime::new(
        config,
        CaptureLlm {
            captured: store.clone(),
        },
    );
    for mut m in operation_specs::build_manifests(
        &vec!["agent-dev".to_string()],
        "http://127.0.0.1:1/execute",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ) {
        let mid = m.compute_manifest_id().unwrap();
        m.manifest_id = mid.clone();
        j.register_harness_manifest(&m).unwrap();
        j.enable_harness(&ApprovedHarnessChange {
            intent: HarnessChangeIntent {
                action: HarnessChangeAction::Enable,
                manifest_id: mid.clone(),
                expected_snapshot_id: j.current_registry_snapshot_id().unwrap(),
                requested_by: "ipc_operator".into(),
            },
            decision_id: "decision_test".into(),
        })
        .unwrap();
    }
    let envelope_val = serde_json::json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": "e1",
        "received_at": "2026-01-01T00:00:00Z",
        "payload": {
            "sender_open_id": "owner",
            "sender_type": "user",
            "chat_id": "c1",
            "chat_type": "p2p",
            "message_id": "m1",
            "message_type": "text",
            "text": "test",
            "mentions": []
        },
        "auth_context": { "authenticated": true },
        "routing_hint": {},
    });
    let event = g
        .validate_ingress(&j, serde_json::from_value(envelope_val).unwrap())
        .unwrap();
    let outcome = runtime.deliver(&j, &g, event).unwrap();
    if let Ok(Some(leased)) = j.lease_next_outbox_dispatch() {
        j.succeed_outbox_dispatch(
            &agent_core_kernel::domain::Receipt {
                invocation_id: leased.invocation_id,
                status: agent_core_kernel::domain::ReceiptStatus::Succeeded,
                output: serde_json::json!({"text": "ok"}),
                external_ref: None,
                occurred_at: chrono::Utc::now(),
            },
            &outcome.run_id,
            leased.session_id.as_ref(),
        )
        .unwrap();
    }
    let captured = store.lock().unwrap();
    assert!(
        !captured.is_empty(),
        "provider_tools captured for runtime deliver"
    );
    let fn_tool = |name: &str| {
        captured
            .iter()
            .find(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    == Some(name)
            })
            .unwrap()
    };
    let cp_params = fn_tool("external.coding_capability_propose")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert!(!fn_tool("external.coding_capability_propose")
        .get("function")
        .unwrap()
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty());
    assert_eq!(
        cp_params.get("type").and_then(|v| v.as_str()),
        Some("object")
    );
    assert_eq!(
        cp_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let cp_req: Vec<&str> = cp_params
        .get("required")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        cp_req.contains(&"workspace_id")
            && cp_req.contains(&"artifact_path")
            && cp_req.contains(&"manifest_path")
            && cp_req.contains(&"evidence_path")
    );
    let cp_ev = cp_params
        .pointer("/properties/workspace_id")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(cp_ev.contains(&serde_json::json!("agent-dev")));
    let write_params = fn_tool("external.coding_workspace_write")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert_eq!(
        write_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let wmode = write_params
        .pointer("/properties/mode")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        wmode.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
        vec!["replace", "append"]
    );
    let submit_params = fn_tool("external.coding_task_submit")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert_eq!(
        submit_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let sbe = submit_params
        .pointer("/properties/backend")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        sbe.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
        vec!["opencode"]
    );
    let ts_params = fn_tool("external.coding_task_status")
        .get("function")
        .unwrap()
        .get("parameters")
        .unwrap();
    assert_eq!(
        ts_params
            .get("additionalProperties")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    let ts_req: Vec<&str> = ts_params
        .get("required")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(ts_req.contains(&"task_id") && !ts_req.contains(&"workspace_id"));
    assert!(j.verify_hash_chain().unwrap());
}
