//! Runtime-owned model invocation telemetry.
//!
//! This is the only wrapper around `LlmClient::complete` used by the Runtime.
//! It writes a started fact before the call and exactly one replay-safe terminal
//! fact after it. Telemetry is selected from sanitized provider metadata and
//! never includes the input blocks, user prompt, response content, or raw error.

use super::Runtime;
use crate::domain::{JournalEventKind, Run, Session};
use crate::journal::JournalStore;
use crate::llm::{LlmClient, LlmInput, LlmOutput, ModelMaterialization};
use crate::runtime::hook_call::{call_context_artifact_hook, ContextArtifactOutcome};
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
        deadline: Option<std::time::Instant>,
    ) -> Result<LlmOutput> {
        // Run deadline guard (High 2): do not START a model call once the
        // frozen Run budget deadline has passed.
        if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!("run_deadline_exceeded:no_new_llm_invocation"));
            }
        }
        let invocation_id = format!("model:{}:{round_index}", run.id.0);
        let receipt_id = format!("model-receipt:{invocation_id}");
        let requested_provider = safe_label(self.llm.provider_hint(), "unknown");
        let requested_model = safe_label(self.llm.model_hint(), "unknown");
        let profile = safe_label(&self.config.agent_id.0, "default");
        let candidate = self.llm.stage_candidate(input)?;
        let context = call_context_artifact_hook(
            &candidate,
            self.hook_client.as_deref(),
            self.hook_config.as_ref(),
            journal,
            run,
            session,
            round_index,
        )?;
        let (artifacts, context_provider_id, context_request_id, candidate_digest, hook_status) =
            match context {
                ContextArtifactOutcome::Provider {
                    artifacts,
                    provider_id,
                    request_id,
                    candidate_digest,
                } => (
                    artifacts,
                    provider_id,
                    Some(request_id),
                    candidate_digest,
                    "provider",
                ),
                ContextArtifactOutcome::Candidate { artifact, status } => {
                    let digest = artifact.digest.clone();
                    (
                        vec![artifact],
                        "kernel-bound-candidate".into(),
                        None,
                        digest,
                        status,
                    )
                }
                ContextArtifactOutcome::Terminate { error_code } => {
                    return Err(anyhow!("context_hook_terminated:{error_code}"));
                }
            };
        let artifact_digests = artifacts
            .iter()
            .map(|artifact| artifact.digest.clone())
            .collect::<Vec<_>>();
        let materialized = self.llm.materialize_context(&candidate, &artifacts)?;
        let started_at = Utc::now();
        let started_text = started_at.to_rfc3339_opts(SecondsFormat::Millis, true);
        let (input, budget) = match materialized {
            ModelMaterialization::Ready { input, budget } => (input, budget),
            ModelMaterialization::OverBudget { budget } => {
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
                        "model_call_permitted": false,
                        "context_provider_id": context_provider_id,
                        "context_request_id": context_request_id,
                        "context_hook_status": hook_status,
                        "candidate_digest": candidate_digest,
                        "artifact_digests": artifact_digests,
                        "input_tokens": budget.input_tokens,
                        "reserved_output_tokens": budget.reserved_output_tokens,
                        "context_window_tokens": budget.context_window_tokens,
                    }),
                )?;
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
                        "finished_at": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                        "latency_ms": 0,
                        "error_category": "model_input_over_budget",
                        "round_index": round_index,
                        "model_called": false,
                        "input_tokens": budget.input_tokens,
                        "reserved_output_tokens": budget.reserved_output_tokens,
                        "context_window_tokens": budget.context_window_tokens,
                    }),
                )?;
                return Err(anyhow!("model_input_over_budget"));
            }
        };
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
                "model_call_permitted": true,
                "context_provider_id": context_provider_id,
                "context_request_id": context_request_id,
                "context_hook_status": hook_status,
                "candidate_digest": candidate_digest,
                "artifact_digests": artifact_digests,
                "input_tokens": budget.input_tokens,
                "reserved_output_tokens": budget.reserved_output_tokens,
                "context_window_tokens": budget.context_window_tokens,
            }),
        )?;

        // Bind the remaining Run deadline into the invocation so the network
        // client stops waiting at the deadline (effective timeout =
        // min(client timeout, remaining)). A call that overruns the deadline
        // is reported as model_timeout; the caller stops and applies the
        // frozen yield/terminate semantics instead of waiting for the natural
        // return.
        let mut input = input;
        if let Some(deadline) = deadline {
            let remaining_ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64;
            if remaining_ms > 0 {
                input.timeout_override_ms =
                    Some(remaining_ms.min(self.config.model_timeout_ms.max(1)));
            }
        }
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
