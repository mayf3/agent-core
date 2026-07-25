//! Runtime-owned model invocation telemetry.
//!
//! This is the only wrapper around `LlmClient::complete` used by the Runtime.
//! It writes a started fact before the call and exactly one replay-safe terminal
//! fact after it. Telemetry is selected from sanitized provider metadata and
//! never includes the input blocks, user prompt, response content, or raw error.

use super::Runtime;
use crate::domain::{JournalEventKind, Run, Session};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput};
use anyhow::{anyhow, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use std::time::Instant;

impl<L: LlmClient + 'static> Runtime<L> {
    pub(super) fn complete_model_invocation(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        round_index: usize,
        input: LlmInput,
    ) -> Result<LlmOutput> {
        let invocation_id = format!("model:{}:{round_index}", run.id.0);
        let receipt_id = format!("model-receipt:{invocation_id}");
        let requested_provider = safe_label(self.llm.provider_hint(), "unknown");
        let requested_model = safe_label(self.llm.model_hint(), "unknown");
        let profile = safe_label(&self.config.agent_id.0, "default");
        let started_at = Utc::now();
        let started_text = started_at.to_rfc3339_opts(SecondsFormat::Millis, true);
        journal.record_model_invocation_event(
            JournalEventKind::ModelInvocationStarted,
            &run.id,
            &session.id,
            &invocation_id,
            json!({
                "schema_version": "model.invocation.started.v0",
                "run_id": run.id.0,
                "invocation_id": invocation_id,
                "profile": profile,
                "requested_provider": requested_provider,
                "requested_model": requested_model,
                "started_at": started_text,
                "round_index": round_index,
            }),
        )?;

        let timer = Instant::now();
        let result = self.llm.complete(input);
        let latency_ms = timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let finished_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut output = match result {
            Ok(output) => output,
            Err(_) => {
                journal.record_model_invocation_event(
                    JournalEventKind::ModelInvocationFailed,
                    &run.id,
                    &session.id,
                    &invocation_id,
                    json!({
                        "schema_version": "model.invocation.failed.v0",
                        "run_id": run.id.0,
                        "invocation_id": invocation_id,
                        "receipt_id": receipt_id,
                        "profile": profile,
                        "provider": requested_provider,
                        "model": requested_model,
                        "started_at": started_text,
                        "finished_at": finished_at,
                        "latency_ms": latency_ms,
                        "error_category": "model_client_error",
                        "round_index": round_index,
                    }),
                )?;
                return Err(anyhow!("model invocation failed"));
            }
        };

        let provider = safe_label(&output.provider, "unknown");
        let model = safe_label(&output.model, "unknown");
        let failure = output.failure_category().map(safe_category);
        let terminal_kind = if failure.is_some() {
            JournalEventKind::ModelInvocationFailed
        } else {
            JournalEventKind::ModelInvocationCompleted
        };
        let terminal_payload = if let Some(error_category) = failure {
            json!({
                "schema_version": "model.invocation.failed.v0",
                "run_id": run.id.0,
                "invocation_id": invocation_id,
                "receipt_id": receipt_id,
                "profile": profile,
                "provider": provider,
                "model": model,
                "started_at": started_text,
                "finished_at": finished_at,
                "latency_ms": latency_ms,
                "error_category": error_category,
                "round_index": round_index,
            })
        } else {
            let usage = output.normalized_usage();
            json!({
                "schema_version": "model.invocation.completed.v0",
                "run_id": run.id.0,
                "invocation_id": invocation_id,
                "receipt_id": receipt_id,
                "profile": profile,
                "provider": provider,
                "model": model,
                "started_at": started_text,
                "finished_at": finished_at,
                "latency_ms": latency_ms,
                "input_tokens": usage.input_tokens,
                "cached_input_tokens": usage.cached_input_tokens,
                "output_tokens": usage.output_tokens,
                "reasoning_tokens": usage.reasoning_tokens,
                "total_tokens": usage.total_tokens,
                "finish_reason": output.finish_reason().and_then(safe_optional_label),
                "error_category": null,
                "estimated_cost": usage.estimated_cost,
                "provider_usage_extensions": usage.provider_usage_extensions,
                "round_index": round_index,
            })
        };
        let terminal = journal.record_model_invocation_event(
            terminal_kind,
            &run.id,
            &session.id,
            &invocation_id,
            terminal_payload,
        )?;

        bind_legacy_receipt(
            &mut output.journal_payload,
            &invocation_id,
            &receipt_id,
            &terminal.event_id.0,
        );
        journal.append_event(
            JournalEventKind::LlmCompleted,
            Some(&run.id),
            Some(&session.id),
            Some(&invocation_id),
            output.journal_payload.clone(),
        )?;
        Ok(output)
    }
}

fn bind_legacy_receipt(
    payload: &mut Value,
    invocation_id: &str,
    receipt_id: &str,
    receipt_event_id: &str,
) {
    if !payload.is_object() {
        let legacy = std::mem::replace(payload, json!({}));
        *payload = json!({"legacy_payload": legacy});
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert("model_invocation_id".into(), json!(invocation_id));
        object.insert("model_receipt_id".into(), json!(receipt_id));
        object.insert("model_receipt_event_id".into(), json!(receipt_event_id));
    }
}

fn safe_label(value: &str, fallback: &str) -> String {
    safe_optional_label(value).unwrap_or_else(|| fallback.to_string())
}

fn safe_optional_label(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(|character| character.is_control())
    {
        return None;
    }
    Some(value.to_string())
}

fn safe_category(value: &str) -> String {
    match value {
        "model_config_required"
        | "model_response_parse_failed"
        | "model_timeout"
        | "model_request_failed" => value.to_string(),
        value
            if value.strip_prefix("model_http_").is_some_and(|code| {
                code.len() == 3 && code.chars().all(|c| c.is_ascii_digit())
            }) =>
        {
            value.to_string()
        }
        _ => "model_request_failed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KernelConfig;
    use crate::domain::{
        AgentId, ChannelKind, EventId, JournalEvent, PrincipalId, PrincipalSource,
        PrincipalSubject, RunId, RunMode, RunPrincipal, RunStatus, SessionId, SessionStatus,
    };
    use crate::llm::ToolCallResult;
    use anyhow::{bail, Result};
    use serde_json::json;
    use std::path::PathBuf;

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
            context_recent_messages: 6,
            context_max_block_chars: 4_000,
            outbox_dispatcher_enabled: false,
            outbox_dispatcher_poll_interval_ms: 10,
            extra_allowed_operations: vec![],
            require_write_approval: false,
            write_approval_ttl_secs: 0,
            fallback_tool_name_indexed: false,
            primary_tool_name_indexed: false,
            harness_read_timeout_ms: 10_000,
            harness_artifact_root: std::env::temp_dir(),
            coding_harness_api_url: "http://127.0.0.1:7200".into(),
            coding_harness_artifact_digest:
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
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
