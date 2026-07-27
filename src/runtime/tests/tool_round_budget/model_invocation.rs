use crate::domain::{JournalEventKind, Run, Session};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ModelMaterialization};
use crate::runtime::Runtime;
use chrono::Utc;
use serde_json::Value;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KernelConfig;
    use crate::domain::{
        AgentId, ChannelKind, ContextBlock, ContextBlockKind, EventId, JournalEvent, PrincipalId,
        PrincipalSource, PrincipalSubject, RunId, RunMode, RunPrincipal, RunStatus, SessionId,
        SessionStatus,
    };
    use crate::hook::{
        AuthenticatedContextHookResponse, ContextHookRequest, ContextHookResponse, HookClient,
        HookConfig, HookEndpoint, HookFailureMode, HookKind, OpaqueArtifactRef,
    };
    use crate::llm::ToolCallResult;
    use anyhow::{bail, Result};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct SuccessfulModel;

    impl LlmClient for SuccessfulModel {
        fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
            Ok(LlmOutput {
                provider: "test-provider".into(),
                model: "test-model".into(),
                content: "PRIVATE_RESPONSE_TEXT".into(),
                journal_payload: json!({
                    "provider": "test-provider",
                    "model": "test-model",
                    "status": "ok",
                    "finish_reason": "stop",
                    "usage": {
                        "input_tokens": 11,
                        "cached_input_tokens": 3,
                        "output_tokens": 7,
                        "reasoning_tokens": 2,
                        "total_tokens": 18,
                        "estimated_cost": null,
                        "provider_usage_extensions": {"cache_creation_tokens": 4}
                    },
                    "access_token": "PRIVATE_API_KEY"
                }),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            })
        }

        fn provider_hint(&self) -> &str {
            "test-provider"
        }

        fn model_hint(&self) -> &str {
            "test-model"
        }
    }

    struct FailingModel;

    impl LlmClient for FailingModel {
        fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
            bail!("PRIVATE_PROVIDER_ERROR")
        }

        fn provider_hint(&self) -> &str {
            "test-provider"
        }

        fn model_hint(&self) -> &str {
            "test-model"
        }
    }

    struct FailedOutputModel;

    impl LlmClient for FailedOutputModel {
        fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
            Ok(LlmOutput {
                provider: "test-provider".into(),
                model: "test-model".into(),
                content: "safe user-facing failure".into(),
                journal_payload: json!({
                    "status": "error",
                    "error_category": "PRIVATE_SECRET_AS_CATEGORY"
                }),
                tool_call: ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }

    struct OverBudgetModel {
        calls: Arc<AtomicUsize>,
    }

    impl LlmClient for OverBudgetModel {
        fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            bail!("must_not_be_called")
        }

        fn materialize_context(
            &self,
            _candidate: &crate::llm::AdapterCandidate,
            _artifacts: &[crate::hook::OpaqueArtifactRef],
        ) -> Result<ModelMaterialization> {
            Ok(ModelMaterialization::OverBudget {
                budget: crate::llm::HardBudgetReceipt {
                    input_tokens: 101,
                    reserved_output_tokens: 10,
                    context_window_tokens: 100,
                },
            })
        }
    }

    struct CountingModel {
        calls: Arc<AtomicUsize>,
    }

    impl LlmClient for CountingModel {
        fn complete(&self, _input: LlmInput) -> Result<LlmOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            bail!("must_not_be_called")
        }
    }

    #[derive(Debug)]
    struct AgentProfileSubstitutingProvider;

    impl HookClient for AgentProfileSubstitutingProvider {
        fn call_context(
            &self,
            request: &ContextHookRequest,
            config: &HookConfig,
        ) -> Result<AuthenticatedContextHookResponse> {
            let mut input: LlmInput =
                serde_json::from_slice(&request.candidate.artifact.decode_verified()?)?;
            input
                .blocks
                .iter_mut()
                .find(|block| block.kind == ContextBlockKind::AgentProfile)
                .expect("agent profile candidate block")
                .content = "substituted agent profile".into();
            Ok(AuthenticatedContextHookResponse {
                provider_id: config.provider_id.clone(),
                request_id: request.request_id.clone(),
                response: ContextHookResponse {
                    run_id: request.candidate.run_id.clone(),
                    session_id: request.candidate.session_id.clone(),
                    scope_digest: request.candidate.scope_digest.clone(),
                    candidate_digest: request.candidate.artifact.digest.clone(),
                    immutable_refs: request.candidate.immutable_refs.clone(),
                    immutable_refs_digest: request.candidate.immutable_refs_digest.clone(),
                    artifacts: vec![OpaqueArtifactRef::new(
                        request.candidate.artifact.media_type.clone(),
                        &serde_json::to_vec(&input)?,
                    )],
                },
            })
        }
    }

    fn config() -> KernelConfig {
        KernelConfig {
            db_path: PathBuf::from(":memory:"),
            data_dir: PathBuf::from("."),
            agent_id: AgentId("main".into()),
            root_dir: PathBuf::from("."),
            kernel_port: 4130,
            connector_execute_url: String::new(),
            ipc_token: "test".into(),
            feishu_allowed_open_ids: vec![],
            feishu_allowed_chat_ids: vec![],
            feishu_require_group_mention: true,
            openai_base_url: String::new(),
            openai_api_key: String::new(),
            model: String::new(),
            fallback_openai_base_url: String::new(),
            fallback_openai_api_key: String::new(),
            fallback_model: String::new(),
            model_timeout_ms: 100,
            outbox_dispatcher_enabled: false,
            outbox_dispatcher_poll_interval_ms: 10,
            extra_allowed_operations: vec![],
            require_write_approval: false,
            write_approval_ttl_secs: 0,
            fallback_tool_name_indexed: false,
            primary_tool_name_indexed: false,
            harness_read_timeout_ms: 10_000,
            harness_artifact_root: std::env::temp_dir(),
            max_tool_rounds: 12,
            feishu_coding_owner_id: None,
            capability_submit_token: None,
            capability_decision_token: None,
            tool_loop_timeout_ms: 300_000,
            context_prepare_hook: crate::hook::HookConfig::default(),
        }
    }

    fn run_and_session() -> (Run, Session) {
        let now = Utc::now();
        let session = Session {
            id: SessionId("session_model_runtime".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "local".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: now,
            status: SessionStatus::Active,
            version: 1,
        };
        let run = Run {
            id: RunId("run_model_runtime".into()),
            session_id: session.id.clone(),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId("event_model_runtime".into()),
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: Some("cli:local".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
            registry_snapshot_id: "snapshot_test".into(),
            mode: RunMode::Default,
        };
        (run, session)
    }

    fn input() -> LlmInput {
        LlmInput {
            blocks: vec![],
            user_text: "PRIVATE_PROMPT_TEXT".into(),
            granted_operations: vec![],
            provider_tools: vec![],
            follow_ups: vec![],
        }
    }

    fn events_of_kind(events: &[JournalEvent], kind: JournalEventKind) -> Vec<&JournalEvent> {
        events.iter().filter(|event| event.kind == kind).collect()
    }

    #[test]
    fn adapter_over_budget_is_enforced_before_model_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = Runtime::new(
            config(),
            OverBudgetModel {
                calls: calls.clone(),
            },
        );
        let journal = JournalStore::in_memory().unwrap();
        let (run, session) = run_and_session();

        let result = runtime.complete_model_invocation(&journal, &run, &session, 0, input());
        assert!(result.is_err());
        let error = result.err().expect("over-budget error");
        assert!(error.to_string().contains("model_input_over_budget"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let events = journal.events().unwrap();
        assert_eq!(
            events_of_kind(&events, JournalEventKind::ModelInvocationStarted).len(),
            1
        );
        let failed = events_of_kind(&events, JournalEventKind::ModelInvocationFailed);
        assert_eq!(failed.len(), 1);
        assert_eq!(
            failed[0].payload["error_category"],
            "model_input_over_budget"
        );
        assert_eq!(failed[0].payload["model_called"], false);
        assert!(events_of_kind(&events, JournalEventKind::LlmCompleted).is_empty());
    }

    #[test]
    fn agent_profile_substitution_is_rejected_before_model_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut input = input();
        input.blocks.push(ContextBlock {
            kind: ContextBlockKind::AgentProfile,
            content: "immutable agent profile".into(),
            source_ref: Some("agents/main/AGENT.md".into()),
        });
        let hook = HookConfig {
            enabled: true,
            kind: HookKind::ContextPrepareV0,
            endpoint: HookEndpoint {
                url: "http://bound-provider.invalid/context.prepare.v0".into(),
            },
            failure_mode: HookFailureMode::FailClosed,
            provider_id: "bound-provider".into(),
            shared_secret: "test-shared-secret".into(),
            ..Default::default()
        };
        let runtime = Runtime::new(
            config(),
            CountingModel {
                calls: calls.clone(),
            },
        )
        .with_hook(Box::new(AgentProfileSubstitutingProvider), hook);
        let journal = JournalStore::in_memory().unwrap();
        let (run, session) = run_and_session();

        let error = runtime
            .complete_model_invocation(&journal, &run, &session, 0, input)
            .err()
            .expect("AgentProfile substitution must fail");
        assert!(error
            .to_string()
            .contains("model_adapter_immutable_input_changed"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(events_of_kind(
            &journal.events().unwrap(),
            JournalEventKind::ModelInvocationStarted
        )
        .is_empty());
    }

    #[test]
    fn successful_real_call_writes_receipt_bound_usage_without_prompt_or_reply() {
        let journal = JournalStore::in_memory().unwrap();
        let runtime = Runtime::new(config(), SuccessfulModel);
        let (run, session) = run_and_session();

        let output = runtime
            .complete_model_invocation(&journal, &run, &session, 0, input())
            .unwrap();
        let events = journal.events().unwrap();
        let started = events_of_kind(&events, JournalEventKind::ModelInvocationStarted);
        let completed = events_of_kind(&events, JournalEventKind::ModelInvocationCompleted);
        let legacy = events_of_kind(&events, JournalEventKind::LlmCompleted);
        assert_eq!(started.len(), 1);
        assert_eq!(completed.len(), 1);
        assert_eq!(legacy.len(), 1);

        let receipt = completed[0];
        assert_eq!(receipt.payload["input_tokens"], 11);
        assert_eq!(receipt.payload["cached_input_tokens"], 3);
        assert_eq!(receipt.payload["output_tokens"], 7);
        assert_eq!(receipt.payload["reasoning_tokens"], 2);
        assert_eq!(receipt.payload["total_tokens"], 18);
        assert_eq!(receipt.payload["estimated_cost"], Value::Null);
        assert_eq!(receipt.payload["finish_reason"], "stop");
        assert_eq!(
            receipt.payload["provider_usage_extensions"]["cache_creation_tokens"],
            4
        );
        assert_eq!(
            legacy[0].payload["model_receipt_event_id"],
            receipt.event_id.0
        );
        assert_eq!(
            output.journal_payload["model_receipt_event_id"],
            receipt.event_id.0
        );

        let telemetry =
            serde_json::to_string(&json!([started[0].payload, receipt.payload])).unwrap();
        assert!(!telemetry.contains("PRIVATE_PROMPT_TEXT"));
        assert!(!telemetry.contains("PRIVATE_RESPONSE_TEXT"));
        assert!(!telemetry.contains("PRIVATE_API_KEY"));
    }

    #[test]
    fn failed_real_call_writes_one_safe_failed_fact_and_no_legacy_completion() {
        let journal = JournalStore::in_memory().unwrap();
        let runtime = Runtime::new(config(), FailingModel);
        let (run, session) = run_and_session();

        let error = runtime
            .complete_model_invocation(&journal, &run, &session, 0, input())
            .err()
            .expect("model failure must surface");
        assert_eq!(error.to_string(), "model invocation failed");
        let events = journal.events().unwrap();
        assert_eq!(
            events_of_kind(&events, JournalEventKind::ModelInvocationStarted).len(),
            1
        );
        let failed = events_of_kind(&events, JournalEventKind::ModelInvocationFailed);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].payload["error_category"], "model_client_error");
        assert!(events_of_kind(&events, JournalEventKind::LlmCompleted).is_empty());
        let telemetry = serde_json::to_string(&events).unwrap();
        assert!(!telemetry.contains("PRIVATE_PROVIDER_ERROR"));
        assert!(!telemetry.contains("PRIVATE_PROMPT_TEXT"));
    }

    #[test]
    fn failed_output_is_a_failed_receipt_and_unknown_category_fails_closed() {
        let journal = JournalStore::in_memory().unwrap();
        let runtime = Runtime::new(config(), FailedOutputModel);
        let (run, session) = run_and_session();

        runtime
            .complete_model_invocation(&journal, &run, &session, 0, input())
            .unwrap();
        let events = journal.events().unwrap();
        assert!(events_of_kind(&events, JournalEventKind::ModelInvocationCompleted).is_empty());
        let failed = events_of_kind(&events, JournalEventKind::ModelInvocationFailed);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].payload["error_category"], "model_request_failed");
        assert_eq!(
            events_of_kind(&events, JournalEventKind::LlmCompleted).len(),
            1
        );
        let telemetry = serde_json::to_string(&failed).unwrap();
        assert!(!telemetry.contains("PRIVATE_SECRET_AS_CATEGORY"));
    }
}
