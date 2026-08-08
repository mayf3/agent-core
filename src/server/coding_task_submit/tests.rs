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
    use crate::server::coding_harness_client::CodingHarnessExecutionOutcome;
    use chrono::Utc;
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

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
            force_legacy_runtime: false,
            tool_loop_timeout_ms: 300_000,
            context_prepare_hook: HookConfig::default(),
            budget_hook: HookConfig::default(),
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
            budget_hook_id: None,
            budget_hook_version: None,
            budget_decision_digest: None,
            budget_max_tool_rounds: None,
            budget_max_wall_time_ms: None,
            budget_exhaustion_action: None,
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
            CodingHarnessExecutionOutcome::Succeeded(
                accepted_result(&root, &request, &approved.intent().invocation_id.0).unwrap(),
            )
        };
        let first = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:0:0:replay",
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
            "tool:test:0:0:replay",
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

    fn request_digest(request: &DevelopmentRequest) -> String {
        Sha256Digest::compute(&serde_json::to_vec(request).unwrap())
            .as_str()
            .to_string()
    }

    #[test]
    fn running_succeeded_and_unknown_attempts_all_block_a_new_attempt() -> anyhow::Result<()> {
        // B: running blocks a new attempt.
        let (journal, _, _, request, run, session) = fixture()?;
        let digest = request_digest(&request);
        let running = InvocationId("attempt_running".into());
        journal.claim_coding_task_submission(
            &request.source_message_id,
            "tool:test:running:first",
            &digest,
            &running.0,
            &running,
            &run.id,
            &session.id,
            "decision_running_first",
        )?;
        let error = journal
            .claim_coding_task_submission(
                &request.source_message_id,
                "tool:test:running:second",
                &digest,
                "attempt_running_second",
                &InvocationId("attempt_running_second".into()),
                &run.id,
                &session.id,
                "decision_running_second",
            )
            .err()
            .expect("running attempt must block a new attempt");
        assert!(error
            .to_string()
            .contains("CODING_TASK_ALREADY_IN_PROGRESS"));

        // C: succeeded blocks a new attempt.
        let (journal, _, _, request, run, session) = fixture()?;
        let digest = request_digest(&request);
        let succeeded = InvocationId("attempt_succeeded".into());
        journal.claim_coding_task_submission(
            &request.source_message_id,
            "tool:test:succeeded:first",
            &digest,
            &succeeded.0,
            &succeeded,
            &run.id,
            &session.id,
            "decision_succeeded_first",
        )?;
        journal.complete_coding_task_submission(&succeeded.0, &succeeded, &json!({"ok":true}))?;
        journal.complete_coding_task_submission(&succeeded.0, &succeeded, &json!({"ok":true}))?;
        assert_eq!(
            journal
                .events()?
                .into_iter()
                .filter(|event| {
                    event.kind == JournalEventKind::ReceiptReceived
                        && event.correlation_id.as_deref() == Some(&succeeded.0)
                })
                .count(),
            1,
            "idempotent terminal replay must not append a second Receipt"
        );
        let error = journal
            .claim_coding_task_submission(
                &request.source_message_id,
                "tool:test:succeeded:second",
                &digest,
                "attempt_succeeded_second",
                &InvocationId("attempt_succeeded_second".into()),
                &run.id,
                &session.id,
                "decision_succeeded_second",
            )
            .err()
            .expect("succeeded attempt must block a new attempt");
        assert!(error
            .to_string()
            .contains("CODING_SUBMISSION_ALREADY_SUCCEEDED"));

        // D: outcome_unknown blocks a new attempt.
        let (journal, _, _, request, run, session) = fixture()?;
        let digest = request_digest(&request);
        let unknown = InvocationId("attempt_unknown".into());
        journal.claim_coding_task_submission(
            &request.source_message_id,
            "tool:test:unknown:first",
            &digest,
            &unknown.0,
            &unknown,
            &run.id,
            &session.id,
            "decision_unknown_first",
        )?;
        journal.mark_coding_task_submission_outcome_unknown(&unknown.0, &unknown)?;
        let error = journal
            .claim_coding_task_submission(
                &request.source_message_id,
                "tool:test:unknown:second",
                &digest,
                "attempt_unknown_second",
                &InvocationId("attempt_unknown_second".into()),
                &run.id,
                &session.id,
                "decision_unknown_second",
            )
            .err()
            .expect("unknown attempt must block a new attempt");
        assert!(error
            .to_string()
            .contains("CODING_SUBMISSION_OUTCOME_UNKNOWN"));
        Ok(())
    }

    #[test]
    fn claim_and_initial_governance_facts_are_atomic() -> anyhow::Result<()> {
        let (journal, gateway, config, request, run, session) = fixture()?;
        journal.execute_sql_for_test(
            "CREATE TRIGGER fail_submission_approval_fact
             BEFORE INSERT ON journal_events
             WHEN NEW.kind='InvocationApproved'
             BEGIN SELECT RAISE(ABORT, 'forced approval fact failure'); END;",
        )?;
        let calls = AtomicUsize::new(0);
        let error = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:claim-atomic",
            &|_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                CodingHarnessExecutionOutcome::OutcomeUnknown(anyhow::anyhow!("unexpected"))
            },
        )
        .expect_err("forced Journal failure must abort the claim");
        assert!(error.to_string().contains("forced approval fact failure"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let conn = journal.conn.lock().unwrap();
        let attempts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM coding_task_submissions WHERE source_message_id=?1",
            [&request.source_message_id],
            |row| row.get(0),
        )?;
        let facts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM journal_events WHERE correlation_id LIKE 'attempt_%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(attempts, 0);
        assert_eq!(facts, 0);
        Ok(())
    }

    #[test]
    fn terminal_receipt_and_attempt_status_are_atomic() -> anyhow::Result<()> {
        let (journal, gateway, config, request, run, session) = fixture()?;
        journal.execute_sql_for_test(
            "CREATE TRIGGER fail_submission_receipt
             BEFORE INSERT ON journal_events
             WHEN NEW.kind='ReceiptReceived'
             BEGIN SELECT RAISE(ABORT, 'forced receipt failure'); END;",
        )?;
        let error = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:terminal-atomic",
            &|_, _| CodingHarnessExecutionOutcome::DefinitivelyRejected {
                error_code: "GENERIC_REJECTION".into(),
            },
        )
        .expect_err("forced Receipt failure must roll back terminal status");
        assert!(error.to_string().contains("forced receipt failure"));
        let conn = journal.conn.lock().unwrap();
        let (status, invocation_id): (String, String) = conn.query_row(
            "SELECT status,invocation_id FROM coding_task_submissions
             WHERE source_message_id=?1",
            [&request.source_message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(status, "running");
        let receipts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM journal_events
             WHERE correlation_id=?1 AND kind='ReceiptReceived'",
            [&invocation_id],
            |row| row.get(0),
        )?;
        assert_eq!(receipts, 0);
        Ok(())
    }

    #[test]
    fn unknown_fact_and_attempt_status_are_atomic() -> anyhow::Result<()> {
        let (journal, gateway, config, request, run, session) = fixture()?;
        journal.execute_sql_for_test(
            "CREATE TRIGGER fail_submission_unknown_fact
             BEFORE INSERT ON journal_events
             WHEN NEW.kind='CodingSubmissionOutcomeUnknown'
             BEGIN SELECT RAISE(ABORT, 'forced unknown fact failure'); END;",
        )?;
        let error = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:unknown-atomic",
            &|_, _| {
                CodingHarnessExecutionOutcome::OutcomeUnknown(anyhow::anyhow!(
                    "transport boundary unknown"
                ))
            },
        )
        .expect_err("forced unknown fact failure must roll back terminal status");
        assert!(error.to_string().contains("forced unknown fact failure"));
        let conn = journal.conn.lock().unwrap();
        let (status, invocation_id): (String, String) = conn.query_row(
            "SELECT status,invocation_id FROM coding_task_submissions
             WHERE source_message_id=?1",
            [&request.source_message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(status, "running");
        let facts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM journal_events
             WHERE correlation_id=?1 AND kind='CodingSubmissionOutcomeUnknown'",
            [&invocation_id],
            |row| row.get(0),
        )?;
        assert_eq!(facts, 0);
        Ok(())
    }

    #[test]
    fn replay_closes_unprovable_running_attempt_as_unknown() -> anyhow::Result<()> {
        let (journal, gateway, config, request, run, session) = fixture()?;
        let digest = request_digest(&request);
        let invocation = InvocationId("attempt_crash_window".into());
        journal.claim_coding_task_submission(
            &request.source_message_id,
            "tool:test:crash-window",
            &digest,
            &invocation.0,
            &invocation,
            &run.id,
            &session.id,
            "decision_crash_window",
        )?;
        let error = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:crash-window",
            &|approved, _| {
                assert_eq!(approved.intent().invocation_id, invocation);
                CodingHarnessExecutionOutcome::OutcomeUnknown(anyhow::anyhow!(
                    "persisted handler state incomplete"
                ))
            },
        )
        .expect_err("unprovable replay must fail closed");
        assert!(error.to_string().contains("CODING_HARNESS_OUTCOME_UNKNOWN"));
        let conn = journal.conn.lock().unwrap();
        let status: String = conn.query_row(
            "SELECT status FROM coding_task_submissions WHERE attempt_id=?1",
            [&invocation.0],
            |row| row.get(0),
        )?;
        assert_eq!(status, "outcome_unknown");
        let unknown_facts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM journal_events
             WHERE correlation_id=?1 AND kind='CodingSubmissionOutcomeUnknown'",
            [&invocation.0],
            |row| row.get(0),
        )?;
        assert_eq!(unknown_facts, 1);
        Ok(())
    }

    #[test]
    fn definitive_rejection_opens_one_new_auditable_attempt() -> anyhow::Result<()> {
        let (journal, gateway, config, request, run, session) = fixture()?;
        let root = config.harness_artifact_root.clone();
        let calls = AtomicUsize::new(0);
        let attempt_keys = Mutex::new(Vec::new());
        let execute = |approved: &ApprovedInvocation, _: std::time::Duration| {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            attempt_keys.lock().unwrap().push(
                approved.intent().arguments["idempotency_key"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
            if call == 0 {
                CodingHarnessExecutionOutcome::DefinitivelyRejected {
                    error_code: "GENERIC_REQUEST_REJECTED".into(),
                }
            } else {
                CodingHarnessExecutionOutcome::Succeeded(
                    accepted_result(&root, &request, &approved.intent().invocation_id.0).unwrap(),
                )
            }
        };

        // E: the first attempt is a definitive rejection.
        let first_error = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:rejection:attempt-one",
            &execute,
        )
        .expect_err("first attempt is rejected by the Harness");
        assert!(first_error.to_string().contains("GENERIC_REQUEST_REJECTED"));

        // A: replaying the same trusted call returns the recorded rejection
        // and does not execute the Harness again.
        let replay_error = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:rejection:attempt-one",
            &execute,
        )
        .expect_err("same rejected attempt replays its result");
        assert!(replay_error
            .to_string()
            .contains("GENERIC_REQUEST_REJECTED"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // E/G: a distinct trusted tool call, with byte-identical request
        // content, creates a new attempt that really reaches the Harness.
        let second = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:rejection:attempt-two",
            &execute,
        )?;
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // H: after the second attempt succeeds, a third attempt is blocked
        // before the Harness closure can run.
        let third_error = handle_coding_task_submit_with(
            &journal,
            &gateway,
            &config,
            &request,
            &run,
            &session,
            &request.source_message_id,
            "tool:test:rejection:attempt-three",
            &execute,
        )
        .expect_err("success must close the message slot");
        assert!(third_error
            .to_string()
            .contains("CODING_SUBMISSION_ALREADY_SUCCEEDED"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // F: both immutable attempts remain in sequence for audit, including
        // the first rejection reason and the second success result.
        let conn = journal.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT attempt_id,attempt_sequence,status,error_code,result_json
             FROM coding_task_submissions WHERE source_message_id=?1
             ORDER BY attempt_sequence",
        )?;
        let rows = statement
            .query_map([&request.source_message_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, 1);
        assert_eq!(rows[0].2, "definitively_rejected");
        assert_eq!(rows[0].3.as_deref(), Some("GENERIC_REQUEST_REJECTED"));
        assert!(rows[0].4.is_none());
        assert_eq!(rows[1].1, 2);
        assert_eq!(rows[1].2, "succeeded");
        assert!(rows[1].3.is_none());
        assert!(rows[1].4.is_some());
        assert_ne!(rows[0].0, rows[1].0);
        assert_eq!(rows[1].0, second.submit_invocation_id);

        let keys = attempt_keys.lock().unwrap();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
        assert!(keys
            .iter()
            .all(|key| key.starts_with("development-attempt:attempt_")));
        Ok(())
    }
}
