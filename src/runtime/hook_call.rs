//! context.prepare.v0 hook invocation logic and Runtime delivery helpers,
//! extracted from `mod.rs` to keep that file under the 500-line limit.

use crate::domain::*;
use crate::gateway::Gateway;
use crate::hook::{
    compute_operations_digest, digest_immutable_refs, validate_against_ceiling, CandidateInputRef,
    ContextHookRequest, ExhaustionAction, HookClient, HookConfig, HookFailureMode, HookKind,
    OpaqueArtifactRef, RunBudgetDecision, RunBudgetHookRequest,
};
use crate::journal::JournalStore;
use crate::llm::AdapterCandidate;
use crate::registry::snapshot::RegistrySnapshot;
use crate::runtime::RuntimeOutcome;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};

/// Stub: session spawn is not yet enabled.
pub fn session_spawn() -> Result<()> {
    bail!("not_enabled:session.spawn")
}

/// Stub: run yield is not yet enabled.
pub fn run_yield() -> Result<()> {
    bail!("not_enabled:run.yield")
}

pub(crate) enum ContextArtifactOutcome {
    Provider {
        artifacts: Vec<OpaqueArtifactRef>,
        provider_id: String,
        request_id: String,
        candidate_digest: String,
    },
    Candidate {
        artifact: OpaqueArtifactRef,
        status: &'static str,
    },
    Terminate {
        error_code: String,
    },
}

pub(crate) fn call_context_artifact_hook(
    candidate: &AdapterCandidate,
    hook_client: Option<&dyn HookClient>,
    hook_config: Option<&HookConfig>,
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    round_index: usize,
) -> Result<ContextArtifactOutcome> {
    let candidate_artifact = OpaqueArtifactRef::new(&candidate.media_type, &candidate.bytes);
    let scope_digest = model_scope_digest(run, session, round_index);
    let candidate_ref = CandidateInputRef {
        run_id: run.id.0.clone(),
        session_id: session.id.0.clone(),
        scope_digest,
        artifact: candidate_artifact.clone(),
        immutable_refs: candidate.immutable_refs.clone(),
        immutable_refs_digest: digest_immutable_refs(&candidate.immutable_refs),
    };
    candidate_ref.validate()?;

    let (Some(client), Some(config)) = (hook_client, hook_config) else {
        return Ok(ContextArtifactOutcome::Candidate {
            artifact: candidate_artifact,
            status: "not_configured",
        });
    };
    if !config.enabled {
        return Ok(ContextArtifactOutcome::Candidate {
            artifact: candidate_artifact,
            status: "disabled",
        });
    }
    let request_id = format!(
        "context:{}:{}:{}",
        run.id.0,
        round_index,
        uuid::Uuid::new_v4().simple()
    );
    let request = ContextHookRequest {
        request_id: request_id.clone(),
        candidate: candidate_ref,
    };
    let started = std::time::Instant::now();
    let result = (|| {
        if config.kind != HookKind::ContextPrepareV0 {
            bail!("context_hook_kind_mismatch");
        }
        let authenticated = client.call_context(&request, config)?;
        if authenticated.provider_id != config.provider_id {
            bail!("context_provider_binding_mismatch");
        }
        if authenticated.request_id != request_id {
            bail!("context_request_correlation_mismatch");
        }
        authenticated.response.validate_against(&request)?;
        Ok(authenticated)
    })();
    let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    match result {
        Ok(authenticated) => {
            let response = authenticated.response;
            let artifact_digests = response
                .artifacts
                .iter()
                .map(|artifact| artifact.digest.clone())
                .collect::<Vec<_>>();
            journal.append_event(
                JournalEventKind::HookCallRecorded,
                Some(&run.id),
                Some(&session.id),
                Some(&request_id),
                json!({
                    "hook": "context.prepare.v0",
                    "status": "ok",
                    "provider_id": authenticated.provider_id,
                    "request_id": request_id,
                    "round_index": round_index,
                    "scope_digest": response.scope_digest,
                    "candidate_digest": response.candidate_digest,
                    "immutable_refs_digest": response.immutable_refs_digest,
                    "artifact_digests": artifact_digests,
                    "duration_ms": duration_ms,
                }),
            )?;
            Ok(ContextArtifactOutcome::Provider {
                artifacts: response.artifacts,
                provider_id: authenticated.provider_id,
                request_id,
                candidate_digest: response.candidate_digest,
            })
        }
        Err(error) => {
            let error_code = hook_error_code(&error);
            let (status, action, failure_mode) = match config.failure_mode {
                HookFailureMode::FailClosed => ("failed", "terminate", "fail_closed"),
                HookFailureMode::FailOpen => ("degraded", "candidate", "fail_open"),
                HookFailureMode::Degrade => ("degraded", "candidate", "degrade"),
                HookFailureMode::Disabled => ("failed", "terminate", "disabled"),
            };
            journal.append_event(
                JournalEventKind::HookCallRecorded,
                Some(&run.id),
                Some(&session.id),
                Some(&request_id),
                json!({
                    "hook": "context.prepare.v0",
                    "status": status,
                    "provider_id": config.provider_id,
                    "request_id": request_id,
                    "round_index": round_index,
                    "scope_digest": request.candidate.scope_digest,
                    "candidate_digest": candidate_artifact.digest,
                    "immutable_refs_digest": request.candidate.immutable_refs_digest,
                    "failure_mode": failure_mode,
                    "failure_action": action,
                    "error_code": error_code,
                    "duration_ms": duration_ms,
                }),
            )?;
            if config.failure_mode == HookFailureMode::FailClosed
                || config.failure_mode == HookFailureMode::Disabled
            {
                Ok(ContextArtifactOutcome::Terminate { error_code })
            } else {
                Ok(ContextArtifactOutcome::Candidate {
                    artifact: candidate_artifact,
                    status: "provider_failed_degraded",
                })
            }
        }
    }
}

fn model_scope_digest(run: &Run, session: &Session, round_index: usize) -> String {
    let mut hasher = Sha256::new();
    for value in [
        run.id.0.as_str(),
        session.id.0.as_str(),
        run.principal.principal_id.0.as_str(),
        run.registry_snapshot_id.as_str(),
        &round_index.to_string(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    for grant in &run.principal.grants {
        for value in [grant.operation.as_str(), grant.scope.as_str()] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn hook_error_code(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if message.contains("non-empty endpoint") {
        return "endpoint_missing".into();
    }
    for code in [
        "http_timeout",
        "http_connect_error",
        "http_transport_error",
        "response_too_large",
        "request_too_large",
        "invalid_json",
        "provider_proof_missing",
        "provider_authentication_failed",
        "context_hook_kind_mismatch",
        "context_provider_binding_mismatch",
        "context_request_correlation_mismatch",
        "hook_request_id_mismatch",
        "context_response_run_session_mismatch",
        "context_response_scope_mismatch",
        "context_response_candidate_mismatch",
        "context_response_immutable_refs_mismatch",
        "artifact_digest_mismatch",
    ] {
        if message.contains(code) {
            return code.into();
        }
    }
    "context_provider_failed".into()
}

// ═══════════════════════════════════════════════════════════════════════════
// Runtime delivery helpers (echo, create_run, reply_intent)
// ═══════════════════════════════════════════════════════════════════════════

impl<L: crate::llm::LlmClient + 'static> super::Runtime<L> {
    /// Handle a "deliver_echo" request.
    pub fn deliver_echo(
        &self,
        journal: &JournalStore,
        gateway: &Gateway,
        event: ValidatedEvent,
    ) -> Result<RuntimeOutcome> {
        let session = journal.get_or_create_session(&event.session_target)?;
        journal.append_event(JournalEventKind::SessionReady, None, Some(&session.id), Some(&event.event_id.0), json!({
            "session_id": session.id.0, "agent_id": session.agent_id.0,
            "channel": format!("{:?}", session.channel), "conversation_key": session.conversation_key,
        }))?;
        let snapshot_id = journal
            .current_registry_snapshot_id()
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        if snapshot_id.is_empty() {
            anyhow::bail!("registry_snapshot_invalid: snapshot ID is empty");
        }
        let snapshot = journal
            .load_registry_snapshot(&snapshot_id)
            .map_err(|e| anyhow::anyhow!("registry_snapshot_unavailable: {e}"))?;
        let run = self.create_run(journal, &session, &event, &snapshot_id, &snapshot);
        // Resolve and freeze the Run's budget (echo path also gets a budget).
        let budget = self.resolve_run_budget(journal, &run, &session, &snapshot)?;
        let run = Run {
            budget_hook_id: Some(budget.hook_id.clone()),
            budget_hook_version: Some(budget.hook_version.clone()),
            budget_decision_digest: Some(budget.decision.digest()),
            budget_max_tool_rounds: Some(budget.decision.max_tool_rounds),
            budget_max_wall_time_ms: Some(budget.decision.max_wall_time_ms),
            budget_exhaustion_action: Some(budget.decision.exhaustion_action),
            ..run
        };
        journal.insert_run(&run)?;
        journal.append_event(JournalEventKind::RunStarted, Some(&run.id), Some(&session.id),
            Some(&event.event_id.0), json!({"run_id": run.id.0, "trigger_event_id": run.trigger_event_id.0, "principal_id": run.principal.principal_id.0}))?;
        let snap_for_gateway = snapshot;
        let RuntimeEventPayload::UserMessage {
            text,
            message_id,
            chat_id,
        } = event.payload.clone();
        let reply = format!("收到：{text}");
        let intent = self.reply_intent(&run, &session, &reply, message_id, chat_id);
        let correlation_id = intent.invocation_id.0.clone();
        journal.append_event(
            JournalEventKind::InvocationProposed,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "operation": intent.operation, "idempotency_key": intent.idempotency_key,
            }),
        )?;
        let approved = gateway.approve_invocation(intent, &run, &session, &snap_for_gateway)?;
        journal.append_event(
            JournalEventKind::InvocationApproved,
            Some(&run.id),
            Some(&session.id),
            Some(&correlation_id),
            json!({
                "decision_id": approved.decision_id, "operation": approved.intent().operation,
            }),
        )?;
        self.enqueue_or_pause(
            journal,
            &approved,
            &run,
            &session,
            &correlation_id,
            &snap_for_gateway,
        )?;
        Ok(RuntimeOutcome {
            run_id: run.id,
            session_id: session.id,
            output: reply,
        })
    }

    fn is_coding_owner(&self, principal: &RunPrincipal, chat_type: Option<&str>) -> bool {
        super::coding_grants::is_coding_owner(&self.config, principal, chat_type)
    }

    pub(crate) fn create_run(
        &self,
        journal: &JournalStore,
        session: &Session,
        event: &ValidatedEvent,
        snapshot_id: &str,
        snapshot: &RegistrySnapshot,
    ) -> Run {
        let now = Utc::now();
        let mut principal = event.principal.clone();
        let is_owner = self.is_coding_owner(&principal, event.chat_type.as_deref());
        super::coding_grants::augment_grants(&mut principal, snapshot, is_owner);

        // Load explicit external operation grants from the journal.
        // These grants are persisted in external_operation_grants via
        // JournalStore::create_external_operation_grant and are separate
        // from channel-default grants and owner coding grants.
        //
        // conversation_kind is derived from event.chat_type and the session
        // channel to distinguish Feishu private/p2p from group chat.
        // Fail-closed: unrecognized combinations map to "" which matches
        // no grant (conversation_kind has CHECK constraint p2p/group/cli).
        let conversation_kind = match (&session.channel, event.chat_type.as_deref()) {
            (ChannelKind::Cli, _) => "cli",
            (ChannelKind::Feishu, Some("p2p")) => "p2p",
            (ChannelKind::Feishu, Some("group")) => "group",
            _ => "",
        };
        if let Ok(explicit_grants) = journal.load_active_external_operation_grants(
            &principal.principal_id.0,
            &format!("{:?}", session.channel),
            conversation_kind,
            "principal_channel",
            snapshot_id,
        ) {
            for g in explicit_grants {
                if !principal
                    .grants
                    .iter()
                    .any(|gr| gr.operation == g.operation)
                {
                    principal.grants.push(CapabilityGrant {
                        operation: g.operation,
                        scope: g.scope,
                    });
                }
            }
        }

        Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: self.config.agent_id.clone(),
            trigger_event_id: event.event_id.clone(),
            principal,
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
            registry_snapshot_id: snapshot_id.to_string(),
            mode: RunMode::Default,
            budget_hook_id: None,
            budget_hook_version: None,
            budget_decision_digest: None,
            budget_max_tool_rounds: None,
            budget_max_wall_time_ms: None,
            budget_exhaustion_action: None,
        }
    }

    pub(crate) fn reply_intent(
        &self,
        run: &Run,
        session: &Session,
        text: &str,
        message_id: Option<String>,
        chat_id: Option<String>,
    ) -> InvocationIntent {
        if session.channel == ChannelKind::Feishu {
            InvocationIntent {
                invocation_id: InvocationId(format!("reply:{}", run.id.0)),
                run_id: run.id.clone(),
                operation: crate::domain::operation::FEISHU_SEND_MESSAGE.to_string(),
                arguments: json!({"session_id": session.id.0, "message_id": message_id.unwrap_or_default(), "chat_id": chat_id.unwrap_or_default(), "text": text}),
                idempotency_key: Some(format!("feishu-reply:{}", run.id.0)),
            }
        } else {
            InvocationIntent {
                invocation_id: InvocationId(format!("reply:{}", run.id.0)),
                run_id: run.id.clone(),
                operation: crate::domain::operation::STDOUT_SEND_TEXT.to_string(),
                arguments: json!({"session_id": session.id.0, "text": text}),
                idempotency_key: Some(format!("stdout-reply:{}", run.id.0)),
            }
        }
    }
}

pub(crate) fn ensure_nonblank_reply(content: &str) -> String {
    if content.trim().is_empty() {
        "No reply was generated for this turn.".to_string()
    } else {
        content.to_string()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Run Budget Hook resolution (run.budget.resolve.v0)
// ═══════════════════════════════════════════════════════════════════════════

/// The resolved budget decision plus provenance metadata for audit.
/// Frozen onto the Run and never changed mid-Run.
pub(crate) struct ResolvedBudget {
    pub decision: RunBudgetDecision,
    pub hook_id: String,
    pub hook_version: String,
    /// "default" when the built-in default hook was used, "hook" when an
    /// external hook responded.
    pub source: &'static str,
}

/// The default budget hook ID and version. Used when the snapshot's binding
/// is the builtin default. This is an ordinary registered binding — not a
/// second parallel strategy path.
pub const DEFAULT_BUDGET_HOOK_ID: &str = "builtin:run-budget-default-v0";
pub const DEFAULT_BUDGET_HOOK_VERSION: &str = "v0";

impl<L: crate::llm::LlmClient + 'static> super::Runtime<L> {
    /// Resolve the Run's budget from the binding frozen in the Run's pinned
    /// Registry Snapshot. Returns a frozen decision with provenance.
    ///
    /// Selection semantics (boundary-audit close-out):
    /// - The snapshot's generic `hook_bindings` set is the ONLY authority for
    ///   which hook runs. The binding is selected by `contract ==
    ///   "run.budget.resolve.v0"`; missing or duplicated contract → fail
    ///   closed. Kernel env does not pick the hook.
    /// - `Builtin` binding → the Kernel's default decision function.
    /// - `External` binding → the endpoint from the binding, authenticated by
    ///   the local credential whose provider_id matches the binding. A missing
    ///   or mismatched credential → fail closed (never silently fall back).
    /// - Hook call failure applies the configured failure_mode: FailClosed /
    ///   Disabled → Err; FailOpen / Degrade → default decision.
    pub(crate) fn resolve_run_budget(
        &self,
        journal: &JournalStore,
        run: &Run,
        session: &Session,
        snapshot: &RegistrySnapshot,
    ) -> Result<ResolvedBudget> {
        let default_budget = || ResolvedBudget {
            decision: default_budget_decision(&self.config),
            hook_id: DEFAULT_BUDGET_HOOK_ID.to_string(),
            hook_version: DEFAULT_BUDGET_HOOK_VERSION.to_string(),
            source: "default",
        };

        // Emit a RunBudgetResolved journal event for the default hook path.
        let emit_default_event = |journal: &JournalStore,
                                  run: &Run,
                                  session: &Session,
                                  b: &ResolvedBudget|
         -> Result<()> {
            journal.append_event(
                JournalEventKind::RunBudgetResolved,
                Some(&run.id),
                Some(&session.id),
                None,
                json!({
                    "hook": "run.budget.resolve.v0",
                    "hook_id": b.hook_id,
                    "hook_version": b.hook_version,
                    "source": "default",
                    "decision_digest": b.decision.digest(),
                    "max_tool_rounds": b.decision.max_tool_rounds,
                    "max_wall_time_ms": b.decision.max_wall_time_ms,
                    "exhaustion_action": match b.decision.exhaustion_action {
                        ExhaustionAction::Terminate => "terminate",
                        ExhaustionAction::Yield => "yield",
                    },
                }),
            )?;
            Ok(())
        };

        // The snapshot's generic hook binding set is the single authority for
        // hook selection. The budget contract must resolve to exactly one
        // binding; missing or duplicated → fail closed.
        let binding = snapshot
            .hook_binding(crate::registry::snapshot::BUDGET_HOOK_CONTRACT)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "budget_hook_binding_missing_or_ambiguous: snapshot has no unique \
                     run.budget.resolve.v0 hook binding"
                )
            })?;
        if binding.hook_id.trim().is_empty() {
            bail!("budget_hook_binding_invalid: empty hook_id");
        }

        // Builtin binding → the Kernel's default decision function.
        if binding.binding_kind == crate::registry::snapshot::BindingKind::Builtin {
            let b = ResolvedBudget {
                decision: default_budget_decision(&self.config),
                hook_id: binding.hook_id.clone(),
                hook_version: binding.hook_version.clone(),
                source: "default",
            };
            emit_default_event(journal, run, session, &b)?;
            return Ok(b);
        }

        // External binding → the local credential must match the binding's
        // provider identity, and the endpoint comes from the binding, never
        // from env selection.
        let (Some(client), Some(config)) = (&self.hook_client, &self.budget_hook_config.as_ref())
        else {
            bail!("budget_hook_credential_missing: no local budget hook credential configured");
        };
        if !config.enabled || config.kind != HookKind::RunBudgetResolveV0 {
            bail!("budget_hook_credential_missing: budget hook credential disabled");
        }
        if config.provider_id != binding.provider_id {
            bail!(
                "budget_hook_credential_mismatch: binding provider {} != credential provider {}",
                binding.provider_id,
                config.provider_id
            );
        }
        if config.shared_secret.is_empty() {
            bail!("budget_hook_credential_missing: shared secret empty");
        }
        if binding.endpoint.trim().is_empty() {
            bail!("budget_hook_binding_invalid: external binding without endpoint");
        }
        // Build the transport config from the binding + local credential.
        let mut hook_config = (*config).clone();
        hook_config.endpoint.url = binding.endpoint.clone();
        hook_config.provider_id = binding.provider_id.clone();
        hook_config.kind = HookKind::RunBudgetResolveV0;

        let operations: Vec<String> = snapshot.operations.iter().map(|o| o.name.clone()).collect();
        let operations_digest = compute_operations_digest(&operations);
        let request_id = format!("budget:{}:{}", run.id.0, uuid::Uuid::new_v4().simple());
        let request = RunBudgetHookRequest {
            request_id: request_id.clone(),
            principal: run.principal.principal_id.0.clone(),
            session_id: session.id.0.clone(),
            run_id: run.id.0.clone(),
            registry_snapshot_id: run.registry_snapshot_id.clone(),
            operations_digest,
        };

        let started = std::time::Instant::now();
        let result = (|| {
            let authenticated = client.call_budget(&request, &hook_config)?;
            if authenticated.provider_id != hook_config.provider_id {
                bail!("budget_provider_binding_mismatch");
            }
            if authenticated.request_id != request_id {
                bail!("budget_request_correlation_mismatch");
            }
            authenticated.response.validate_against(&request)?;
            Ok(authenticated)
        })();
        let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

        match result {
            Ok(authenticated) => {
                let decision = authenticated.response.decision.clone();
                journal.append_event(
                    JournalEventKind::RunBudgetResolved,
                    Some(&run.id),
                    Some(&session.id),
                    Some(&request_id),
                    json!({
                        "hook": "run.budget.resolve.v0",
                        "hook_id": binding.hook_id,
                        "hook_version": binding.hook_version,
                        "source": "hook",
                        "decision_digest": decision.digest(),
                        "max_tool_rounds": decision.max_tool_rounds,
                        "max_wall_time_ms": decision.max_wall_time_ms,
                        "exhaustion_action": match decision.exhaustion_action {
                            ExhaustionAction::Terminate => "terminate",
                            ExhaustionAction::Yield => "yield",
                        },
                        "duration_ms": duration_ms,
                    }),
                )?;
                Ok(ResolvedBudget {
                    decision,
                    hook_id: binding.hook_id.clone(),
                    hook_version: binding.hook_version.clone(),
                    source: "hook",
                })
            }
            Err(error) => {
                let error_code = budget_error_code(&error);
                let (action, failure_mode) = match hook_config.failure_mode {
                    HookFailureMode::FailClosed => ("fail_closed_terminate", "fail_closed"),
                    HookFailureMode::Disabled => ("disabled_terminate", "disabled"),
                    HookFailureMode::FailOpen => ("fail_open_default", "fail_open"),
                    HookFailureMode::Degrade => ("degrade_default", "degrade"),
                };
                journal.append_event(
                    JournalEventKind::HookCallRecorded,
                    Some(&run.id),
                    Some(&session.id),
                    Some(&request_id),
                    json!({
                        "hook": "run.budget.resolve.v0",
                        "status": if hook_config.failure_mode == HookFailureMode::FailClosed
                            || hook_config.failure_mode == HookFailureMode::Disabled
                        {
                            "failed"
                        } else {
                            "degraded"
                        },
                        "provider_id": hook_config.provider_id,
                        "request_id": request_id,
                        "failure_mode": failure_mode,
                        "failure_action": action,
                        "error_code": error_code,
                        "duration_ms": duration_ms,
                    }),
                )?;
                if hook_config.failure_mode == HookFailureMode::FailClosed
                    || hook_config.failure_mode == HookFailureMode::Disabled
                {
                    bail!("budget_hook_failed_closed:{error_code}");
                }
                // FailOpen / Degrade: use the default decision.
                let d = default_budget();
                journal.append_event(
                    JournalEventKind::RunBudgetResolved,
                    Some(&run.id),
                    Some(&session.id),
                    Some(&request_id),
                    json!({
                        "hook": "run.budget.resolve.v0",
                        "hook_id": d.hook_id,
                        "hook_version": d.hook_version,
                        "source": "default",
                        "fallback_reason": error_code,
                        "decision_digest": d.decision.digest(),
                        "max_tool_rounds": d.decision.max_tool_rounds,
                        "max_wall_time_ms": d.decision.max_wall_time_ms,
                        "exhaustion_action": "yield",
                        "duration_ms": duration_ms,
                    }),
                )?;
                Ok(d)
            }
        }
    }
}

/// The default budget hook: reproduces the pre-V0 effective limits from the
/// Kernel config. `exhaustion_action = Yield` reproduces the "请发送继续"
/// behaviour.
fn default_budget_decision(config: &crate::config::KernelConfig) -> RunBudgetDecision {
    let decision = RunBudgetDecision {
        max_tool_rounds: config.max_tool_rounds as u32,
        max_wall_time_ms: config.tool_loop_timeout_ms,
        exhaustion_action: ExhaustionAction::Yield,
    };
    // The default must always be within the host ceiling. If the config values
    // somehow exceed it (shouldn't happen — env validation catches this at
    // startup), clamp to the ceiling rather than fail.
    if validate_against_ceiling(&decision).is_err() {
        RunBudgetDecision {
            max_tool_rounds: config
                .max_tool_rounds
                .min(crate::hook::HOST_MAX_TOOL_ROUNDS as usize)
                as u32,
            max_wall_time_ms: config
                .tool_loop_timeout_ms
                .min(crate::hook::HOST_MAX_WALL_TIME_MS),
            exhaustion_action: ExhaustionAction::Yield,
        }
    } else {
        decision
    }
}

fn budget_error_code(error: &anyhow::Error) -> String {
    let message = error.to_string();
    for code in [
        "http_timeout",
        "http_connect_error",
        "http_transport_error",
        "http_status_4xx",
        "http_status_5xx",
        "response_too_large",
        "request_too_large",
        "invalid_json",
        "provider_proof_missing",
        "provider_authentication_failed",
        "budget_provider_binding_mismatch",
        "budget_request_correlation_mismatch",
        "budget_response_request_id_mismatch",
        "budget_response_run_id_mismatch",
        "budget_max_tool_rounds_zero",
        "budget_max_tool_rounds_exceeds_ceiling",
        "budget_max_wall_time_ms_zero",
        "budget_max_wall_time_ms_exceeds_ceiling",
        "unsupported_hook_response",
        "hook_request_id_mismatch",
    ] {
        if message.contains(code) {
            return code.into();
        }
    }
    "budget_hook_failed".into()
}
