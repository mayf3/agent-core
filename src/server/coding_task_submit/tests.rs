#[cfg(test)]
#[path = "../coding_private_origin_tests.rs"]
mod private_origin_tests;

#[cfg(test)]
mod component_manifest_tests {
    use super::super::invocable::invocable_manifest;
    use crate::contract_catalog::CONTRACT_CATALOG_VERSION;
    use crate::domain::*;
    use serde_json::{json, Value};

    fn request() -> DevelopmentRequest {
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.example".into(),
        );
        draft.requirements = vec!["provide a bounded invocation".into()];
        draft.required_contracts = vec!["component.invoke.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["trusted contract tests pass".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "session:test".into(),
            "message:test".into(),
            "development:message:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    fn component() -> Value {
        json!({
            "schema_version": "component-artifact-v1",
            "component_id": "external.example",
            "kind": "invocable_capability",
            "profile_id": "invocable-capability-v0",
            "contract_catalog_version": CONTRACT_CATALOG_VERSION,
            "required_contracts": ["component.invoke.v0"],
            "requested_permissions": ["component.invoke"],
            "deployment_profile": "capability-host-v0",
            "capability": {
                "operation_name": "external.example",
                "description": "A bounded example capability.",
                "input_schema": {"type":"object","additionalProperties":false},
                "output_schema": {"type":"object"},
                "idempotent": true
            }
        })
    }

    #[test]
    fn post_gate_manifest_must_match_requested_contracts_and_permissions() {
        let request = request();
        let digest = format!("sha256:{}", "a".repeat(64));
        invocable_manifest(&request, &component(), &digest).unwrap();

        let mut escalated = component();
        escalated["requested_permissions"] = json!(["component.invoke", "deployment.effect"]);
        assert!(invocable_manifest(&request, &escalated, &digest).is_err());
    }
}

mod receipt_workflow_tests {
    use super::super::handler::{handle_coding_task_submit_with, validate_acceptance};
    use crate::capabilities::store::{ContentStore, Sha256Digest};
    use crate::config::KernelConfig;
    use crate::contract_catalog::CONTRACT_CATALOG_VERSION;
    use crate::domain::*;
    use crate::gateway::Gateway;
    use crate::harness::manifest::HarnessManifest;
    use crate::hook::HookConfig;
    use crate::journal::JournalStore;
    use chrono::Utc;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn fixture() -> anyhow::Result<(
        JournalStore,
        Gateway,
        KernelConfig,
        DevelopmentRequest,
        Run,
        Session,
    )> {
        let root = std::env::temp_dir().join(format!(
            "hcr_retirement_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let mut config = KernelConfig {
            db_path: PathBuf::from(":memory:"),
            data_dir: root.clone(),
            agent_id: AgentId("main".into()),
            root_dir: root.clone(),
            kernel_port: 4130,
            connector_execute_url: "http://127.0.0.1:4131/v1/execute".into(),
            ipc_token: "test-ipc".into(),
            capability_submit_token: None,
            capability_decision_token: None,
            feishu_allowed_open_ids: vec![],
            feishu_allowed_chat_ids: vec![],
            feishu_require_group_mention: true,
            openai_base_url: "https://example.invalid/v1".into(),
            openai_api_key: String::new(),
            model: String::new(),
            fallback_openai_base_url: String::new(),
            fallback_openai_api_key: String::new(),
            fallback_model: String::new(),
            model_timeout_ms: 100,
            outbox_dispatcher_enabled: false,
            outbox_dispatcher_poll_interval_ms: 100,
            extra_allowed_operations: vec![],
            require_write_approval: false,
            write_approval_ttl_secs: 0,
            fallback_tool_name_indexed: false,
            primary_tool_name_indexed: false,
            harness_read_timeout_ms: 10_000,
            harness_artifact_root: root.join("artifacts"),
            max_tool_rounds: 12,
            feishu_coding_owner_id: Some("owner".into()),
            tool_loop_timeout_ms: 300_000,
            context_prepare_hook: HookConfig::default(),
        };
        std::fs::create_dir_all(&config.harness_artifact_root)?;
        config.root_dir = root;
        let journal = JournalStore::in_memory()?;
        let snapshot_id = journal.initialize_registry()?;
        let session = journal.get_or_create_session(&SessionTarget {
            agent_id: config.agent_id.clone(),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:owner".into(),
        })?;
        let now = Utc::now();
        let run = Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: config.agent_id.clone(),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:owner".into()),
                subject: PrincipalSubject::FeishuOpenId("owner".into()),
                source: PrincipalSource::Feishu,
                grants: vec![CapabilityGrant {
                    operation: crate::domain::operation::external::TASK_SUBMIT.into(),
                    scope: "current_session".into(),
                }],
                requester_id: Some("feishu:open_id:owner".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
            registry_snapshot_id: snapshot_id,
            mode: RunMode::Default,
        };
        journal.insert_run(&run)?;
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.example".into(),
        );
        draft.requirements = vec!["return a bounded value".into()];
        draft.required_contracts = vec!["component.invoke.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["trusted profile passes".into()];
        let request = DevelopmentRequest::from_draft(
            draft,
            run.principal.principal_id.0.clone(),
            session.id.0.clone(),
            "message-new-flow".into(),
            "development:message-new-flow".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )?;
        let gateway = Gateway::new(config.clone());
        Ok((journal, gateway, config, request, run, session))
    }

    fn accepted_result(
        root: &std::path::Path,
        request: &DevelopmentRequest,
        invocation_id: &str,
    ) -> anyhow::Result<Value> {
        let store = ContentStore::new(root.to_path_buf());
        let artifact = store.store(b"generic artifact")?.as_str().to_string();
        let evidence = store
            .store(br#"{"profile":"passed"}"#)?
            .as_str()
            .to_string();
        let mut manifest = HarnessManifest {
            manifest_id: String::new(),
            harness_id: "capability-host-v0".into(),
            artifact_digest: artifact.clone(),
            protocol_version: "external-harness-v1".into(),
            endpoint: "http://127.0.0.1:7300/execute".into(),
            operation_name: request.name.clone(),
            description: "generic test capability".into(),
            input_schema: json!({
                "type":"object","properties":{},"required":[],
                "additionalProperties":false
            }),
            output_schema: json!({"type":"string"}),
            idempotent: true,
            created_at: Utc::now(),
        };
        manifest.manifest_id = manifest.compute_manifest_id()?;
        let manifest_digest = store
            .store(&serde_json::to_vec(&manifest)?)?
            .as_str()
            .to_string();
        let request_digest = Sha256Digest::compute(&serde_json::to_vec(request)?)
            .as_str()
            .to_string();
        let candidate_digest = format!("sha256:{}", "c".repeat(64));
        let binding = compute_acceptance_binding_digest(
            &request_digest,
            &candidate_digest,
            &artifact,
            &manifest_digest,
            ExternalOutcome::Passed,
            &request.contract_catalog_version,
            &request.build_profile,
            "component-profile-catalog-v1",
        );
        let receipt_digest = compute_external_receipt_digest(
            SCHEMA_VERSION,
            invocation_id,
            "harness:coding-harness-v0",
            &artifact,
            ExternalOutcome::Passed,
            &evidence,
            Some(&binding),
        );
        let receipt = ExternalReceiptEnvelope {
            schema_version: SCHEMA_VERSION.into(),
            invocation_intent_id: invocation_id.into(),
            issuer: "harness:coding-harness-v0".into(),
            subject_digest: artifact.clone(),
            outcome: ExternalOutcome::Passed,
            evidence_digest: evidence.clone(),
            opaque_payload_digest: Some(binding),
            receipt_digest: receipt_digest.clone(),
        };
        Ok(json!({
            "request_id": request.request_id,
            "request_digest": request_digest,
            "candidate_id": "candidate_generic",
            "candidate_digest": candidate_digest,
            "artifact_ref": artifact,
            "artifact_digest": artifact,
            "manifest_ref": manifest.manifest_id,
            "manifest_digest": manifest_digest,
            "evidence_digest": evidence,
            "acceptance_outcome": "passed",
            "contract_catalog_version": request.contract_catalog_version,
            "profile_id": request.build_profile,
            "profile_catalog_version": "component-profile-catalog-v1",
            "acceptance_receipt": receipt,
            "receipt_digest": receipt_digest,
        }))
    }

    #[test]
    fn new_development_flow_creates_receipt_proposal_and_zero_hcr_facts() -> anyhow::Result<()> {
        let (journal, gateway, config, request, run, session) = fixture()?;
        let before = journal.hcr_fact_counts_for_test()?;
        let root = config.harness_artifact_root.clone();
        let execute = |approved: &ApprovedInvocation, _: std::time::Duration| {
            accepted_result(&root, &request, &approved.intent().invocation_id.0)
        };
        let first = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            &execute,
        )?;
        let replay = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            &execute,
        )?;
        assert_eq!(first.proposal_id, replay.proposal_id);
        assert!(journal
            .load_proposal_receipt_link(&first.proposal_id)?
            .is_some());
        assert!(journal
            .load_capability_approval_by_proposal(&first.proposal_id)?
            .is_some());
        assert!(journal
            .execute_sql_for_test(&format!(
                "UPDATE capability_proposal_receipt_links
                 SET request_digest='sha256:{}' WHERE proposal_id='{}'",
                "f".repeat(64),
                first.proposal_id
            ))
            .is_err());
        assert_eq!(journal.hcr_fact_counts_for_test()?, before);
        Ok(())
    }

    #[test]
    fn acceptance_receipt_rejects_tamper_wrong_issuer_and_wrong_request() -> anyhow::Result<()> {
        let (_journal, _gateway, config, request, _run, _session) = fixture()?;
        let invocation = "invocation_test";
        let valid = accepted_result(&config.harness_artifact_root, &request, invocation)?;
        let request_digest = Sha256Digest::compute(&serde_json::to_vec(&request)?)
            .as_str()
            .to_string();
        assert!(validate_acceptance(&valid, &request, &request_digest, invocation).is_ok());

        let mut tampered = valid.clone();
        tampered["acceptance_receipt"]["subject_digest"] =
            json!(format!("sha256:{}", "f".repeat(64)));
        assert!(validate_acceptance(&tampered, &request, &request_digest, invocation).is_err());

        let mut swapped_artifact = valid.clone();
        swapped_artifact["artifact_digest"] = json!(format!("sha256:{}", "a".repeat(64)));
        swapped_artifact["artifact_ref"] = swapped_artifact["artifact_digest"].clone();
        assert!(
            validate_acceptance(&swapped_artifact, &request, &request_digest, invocation).is_err()
        );

        let mut wrong_issuer = valid.clone();
        wrong_issuer["acceptance_receipt"]["issuer"] = json!("harness:untrusted");
        assert!(validate_acceptance(&wrong_issuer, &request, &request_digest, invocation).is_err());

        let mut wrong_request = valid;
        wrong_request["request_digest"] = json!(format!("sha256:{}", "e".repeat(64)));
        assert!(
            validate_acceptance(&wrong_request, &request, &request_digest, invocation).is_err()
        );
        Ok(())
    }
}
