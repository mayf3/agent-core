use crate::config::KernelConfig;
use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

mod policy;
pub use policy::{evaluate_policy, PolicyVerdict};
mod tool_call;
pub use tool_call::{validate_tool_call, ToolRejection};

#[derive(Clone)]
pub struct Gateway {
    config: KernelConfig,
}

impl Gateway {
    pub fn new(config: KernelConfig) -> Self {
        Self { config }
    }

    pub fn cli_ingress(&self, text: String) -> Result<IngressEnvelope> {
        let text = text.trim().to_string();
        if text.is_empty() {
            bail!("CLI input text is empty");
        }
        Ok(IngressEnvelope {
            protocol_version: "v1".to_string(),
            source: ExternalSource::Cli,
            external_event_id: format!("cli_{}", Uuid::new_v4().simple()),
            received_at: Utc::now(),
            payload: json!({ "text": text }),
            auth_context: AuthContext {
                authenticated: true,
            },
            routing_hint: None,
        })
    }

    pub fn validate_ingress(
        &self,
        journal: &JournalStore,
        envelope: IngressEnvelope,
    ) -> Result<ValidatedEvent> {
        if envelope.protocol_version != "v1" {
            bail!("unsupported protocol version");
        }
        if !envelope.auth_context.authenticated {
            bail!("ingress is not authenticated");
        }
        match envelope.source {
            ExternalSource::Cli => self.validate_cli_ingress(journal, envelope),
            ExternalSource::Feishu => self.validate_feishu_ingress(journal, envelope),
        }
    }

    pub fn approve_invocation(
        &self,
        intent: InvocationIntent,
        run: &Run,
        session: &Session,
    ) -> Result<ApprovedInvocation> {
        // Access control runs through the fixed, pure policy pipeline
        // (Phase 2 M2c); see `src/gateway/policy.rs`. The first denial wins
        // and its reason is surfaced verbatim, preserving the prior error
        // messages (`capability_not_enabled` / `operation_not_allowed` /
        // `target_session_mismatch`).
        match policy::evaluate_policy(&intent, run, session) {
            PolicyVerdict::Deny(reason) => bail!("{reason}"),
            PolicyVerdict::Allow => {}
        }
        // Argument-shape validation is a schema concern (M2a's
        // `argument_schema`, deferred), not an access-control stage, so it
        // stays here rather than in the policy pipeline. The feishu send
        // operation requires message_id / chat_id / text to be present.
        if intent.operation == crate::domain::operation::FEISHU_SEND_MESSAGE {
            string_arg(&intent.arguments, "message_id")?;
            string_arg(&intent.arguments, "chat_id")?;
            string_arg(&intent.arguments, "text")?;
        }
        Ok(ApprovedInvocation::new(
            intent,
            format!("decision_{}", Uuid::new_v4().simple()),
        ))
    }

    pub fn recover_validated_event(&self, event: &JournalEvent) -> Result<ValidatedEvent> {
        let source = string_arg(&event.payload, "source")?;
        let event_id = EventId(string_arg(&event.payload, "event_id")?);
        let dedupe_key = event
            .correlation_id
            .clone()
            .unwrap_or_else(|| string_arg(&event.payload, "dedupe_key").unwrap_or_default());
        match source.as_str() {
            "cli" => self.recover_cli_event(event_id, dedupe_key, &event.payload, event.created_at),
            "feishu" => {
                self.recover_feishu_event(event_id, dedupe_key, &event.payload, event.created_at)
            }
            _ => bail!("unsupported_recovery_source:{source}"),
        }
    }

    fn validate_cli_ingress(
        &self,
        journal: &JournalStore,
        envelope: IngressEnvelope,
    ) -> Result<ValidatedEvent> {
        let text = string_arg(&envelope.payload, "text")?;
        let event_id = EventId::new();
        let source = "cli";
        if !journal.reserve_ingress(source, &envelope.external_event_id, &event_id)? {
            bail!("duplicate_ingress");
        }
        let event = ValidatedEvent {
            event_id: event_id.clone(),
            source: EventSource::Cli,
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".to_string()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: crate::domain::operation::ExecutionProfile::for_channel(
                    ChannelKind::Cli,
                )
                .with_extra(&self.config.extra_allowed_operations)
                .grants,
                requester_id: Some("cli:local".to_string()),
            },
            session_target: SessionTarget {
                agent_id: self.config.agent_id.clone(),
                channel: ChannelKind::Cli,
                conversation_key: "local".to_string(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text: text.clone(),
                message_id: None,
                chat_id: None,
            },
            dedupe_key: format!("{source}:{}", envelope.external_event_id),
            occurred_at: envelope.received_at,
        };
        journal.accept_ingress_with_worker_job(
            &event,
            json!({
                "source": source,
                "external_event_id": envelope.external_event_id,
                "dedupe_key": event.dedupe_key.clone(),
                "event_id": event_id.0,
                "text": text,
                "payload_hash": payload_hash(&envelope.payload),
            }),
        )?;
        Ok(event)
    }

    fn validate_feishu_ingress(
        &self,
        journal: &JournalStore,
        envelope: IngressEnvelope,
    ) -> Result<ValidatedEvent> {
        let text = string_arg(&envelope.payload, "text")?;
        let message_id = string_arg(&envelope.payload, "message_id")?;
        let chat_id = string_arg(&envelope.payload, "chat_id")?;
        let chat_type = string_arg(&envelope.payload, "chat_type")?;
        let sender_open_id = string_arg(&envelope.payload, "sender_open_id")?;
        let sender_type = envelope
            .payload
            .get("sender_type")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let message_type = envelope
            .payload
            .get("message_type")
            .and_then(Value::as_str)
            .unwrap_or("text");
        if sender_type == "app" {
            bail!("skip:bot_sender");
        }
        if message_type != "text" {
            bail!("skip:unsupported_message_type");
        }
        if text.trim().is_empty() {
            bail!("skip:empty_text");
        }
        if chat_type == "p2p"
            && !self.config.feishu_allowed_open_ids.is_empty()
            && !self
                .config
                .feishu_allowed_open_ids
                .iter()
                .any(|id| id == &sender_open_id)
        {
            bail!("skip:sender_not_allowed");
        }
        if chat_type != "p2p"
            && !self.config.feishu_allowed_chat_ids.is_empty()
            && !self
                .config
                .feishu_allowed_chat_ids
                .iter()
                .any(|id| id == &chat_id)
        {
            bail!("skip:chat_not_allowed");
        }
        if chat_type != "p2p"
            && self.config.feishu_require_group_mention
            && !has_mention(&envelope.payload)
        {
            bail!("skip:bot_not_mentioned");
        }
        let event_id = EventId::new();
        let dedupe_id = format!("message:{message_id}");
        if !journal.reserve_ingress("feishu", &dedupe_id, &event_id)? {
            bail!("skip:duplicate_ingress");
        }
        let conversation_key = if chat_type == "p2p" {
            format!("feishu:open_id:{sender_open_id}")
        } else {
            format!("feishu:chat_id:{chat_id}")
        };
        let event = ValidatedEvent {
            event_id: event_id.clone(),
            source: EventSource::Feishu,
            principal: RunPrincipal {
                principal_id: PrincipalId(format!("feishu:open_id:{sender_open_id}")),
                subject: PrincipalSubject::FeishuOpenId(sender_open_id.clone()),
                source: PrincipalSource::Feishu,
                grants: crate::domain::operation::ExecutionProfile::for_channel(
                    ChannelKind::Feishu,
                )
                .with_extra(&self.config.extra_allowed_operations)
                .grants,
                requester_id: Some(format!("feishu:open_id:{sender_open_id}")),
            },
            session_target: SessionTarget {
                agent_id: self.config.agent_id.clone(),
                channel: ChannelKind::Feishu,
                conversation_key: conversation_key.clone(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text,
                message_id: Some(message_id.clone()),
                chat_id: Some(chat_id.clone()),
            },
            dedupe_key: format!("feishu:{dedupe_id}"),
            occurred_at: envelope.received_at,
        };
        journal.accept_ingress_with_worker_job(
            &event,
            json!({
                "source": "feishu",
                "external_event_id": envelope.external_event_id,
                "dedupe_id": dedupe_id,
                "dedupe_key": event.dedupe_key.clone(),
                "event_id": event_id.0,
                "sender_open_id": sender_open_id,
                "chat_id": chat_id,
                "chat_type": chat_type,
                "conversation_key": conversation_key,
                "message_id": message_id,
                "message_type": message_type,
                "text": normalized_text(&envelope.payload),
                "payload_hash": payload_hash(&envelope.payload),
            }),
        )?;
        Ok(event)
    }

    fn recover_cli_event(
        &self,
        event_id: EventId,
        dedupe_key: String,
        payload: &Value,
        occurred_at: DateTime<Utc>,
    ) -> Result<ValidatedEvent> {
        Ok(ValidatedEvent {
            event_id,
            source: EventSource::Cli,
            principal: self.cli_principal(),
            session_target: SessionTarget {
                agent_id: self.config.agent_id.clone(),
                channel: ChannelKind::Cli,
                conversation_key: "local".to_string(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text: string_arg(payload, "text")?,
                message_id: None,
                chat_id: None,
            },
            dedupe_key,
            occurred_at,
        })
    }

    fn recover_feishu_event(
        &self,
        event_id: EventId,
        dedupe_key: String,
        payload: &Value,
        occurred_at: DateTime<Utc>,
    ) -> Result<ValidatedEvent> {
        let sender_open_id = string_arg(payload, "sender_open_id")?;
        let chat_id = string_arg(payload, "chat_id")?;
        let conversation_key = string_arg(payload, "conversation_key")?;
        Ok(ValidatedEvent {
            event_id,
            source: EventSource::Feishu,
            principal: RunPrincipal {
                principal_id: PrincipalId(format!("feishu:open_id:{sender_open_id}")),
                subject: PrincipalSubject::FeishuOpenId(sender_open_id.clone()),
                source: PrincipalSource::Feishu,
                grants: crate::domain::operation::ExecutionProfile::for_channel(
                    ChannelKind::Feishu,
                )
                .with_extra(&self.config.extra_allowed_operations)
                .grants,
                requester_id: Some(format!("feishu:open_id:{sender_open_id}")),
            },
            session_target: SessionTarget {
                agent_id: self.config.agent_id.clone(),
                channel: ChannelKind::Feishu,
                conversation_key,
            },
            payload: RuntimeEventPayload::UserMessage {
                text: string_arg(payload, "text")?,
                message_id: Some(string_arg(payload, "message_id")?),
                chat_id: Some(chat_id),
            },
            dedupe_key,
            occurred_at,
        })
    }

    fn cli_principal(&self) -> RunPrincipal {
        RunPrincipal {
            principal_id: PrincipalId("cli:local".to_string()),
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: crate::domain::operation::ExecutionProfile::for_channel(ChannelKind::Cli)
                .with_extra(&self.config.extra_allowed_operations)
                .grants,
            requester_id: Some("cli:local".to_string()),
        }
    }

    /// Phase 2 M2d: resume a run paused in `AwaitingApproval`. Loads the run's
    /// `ApprovalRequested` snapshot, appends `ApprovalGranted`, queues the
    /// dispatch, and advances the run to `WaitingDispatch`. **Idempotent**: if
    /// the run is not currently `AwaitingApproval` (already resumed/denied, or
    /// never paused), this is a no-op that returns `Ok(())`.
    pub fn approve_run(&self, journal: &JournalStore, run_id: &RunId) -> Result<()> {
        let status = journal.run_status(run_id)?;
        if status.as_deref() != Some("AwaitingApproval") {
            return Ok(());
        }
        let snapshot = journal
            .approval_request_for_run(run_id)?
            .ok_or_else(|| anyhow::anyhow!("approval_request_missing"))?;
        let intent = InvocationIntent {
            invocation_id: InvocationId(
                snapshot
                    .get("invocation_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("approval_request_missing_invocation_id"))?
                    .to_string(),
            ),
            run_id: run_id.clone(),
            operation: snapshot
                .get("operation")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("approval_request_missing_operation"))?
                .to_string(),
            arguments: snapshot
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({})),
            idempotency_key: snapshot
                .get("idempotency_key")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        let session_id = snapshot
            .get("session_id")
            .and_then(Value::as_str)
            .map(|s| SessionId(s.to_string()));
        let decision_id = snapshot
            .get("decision_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let correlation_id = intent.invocation_id.0.clone();
        let approved = ApprovedInvocation::new(intent, decision_id);
        journal.append_event(
            JournalEventKind::ApprovalGranted,
            Some(run_id),
            session_id.as_ref(),
            Some(&correlation_id),
            json!({ "operation": approved.intent().operation }),
        )?;
        journal.queue_outbox_dispatch(&approved, session_id.as_ref())?;
        journal.update_run_status(run_id, "WaitingDispatch")?;
        Ok(())
    }

    /// Phase 2 M2d: deny a run paused in `AwaitingApproval`. Appends
    /// `ApprovalDenied` and fails the run (status `Failed`). **Idempotent**: if
    /// the run is not currently `AwaitingApproval`, this is a no-op `Ok(())`.
    pub fn deny_run(&self, journal: &JournalStore, run_id: &RunId) -> Result<()> {
        let status = journal.run_status(run_id)?;
        if status.as_deref() != Some("AwaitingApproval") {
            return Ok(());
        }
        let snapshot = journal.approval_request_for_run(run_id)?;
        let operation = snapshot
            .as_ref()
            .and_then(|p| p.get("operation"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let session_id = snapshot
            .as_ref()
            .and_then(|p| p.get("session_id"))
            .and_then(Value::as_str)
            .map(|s| SessionId(s.to_string()));
        journal.append_event(
            JournalEventKind::ApprovalDenied,
            Some(run_id),
            session_id.as_ref(),
            None,
            json!({ "operation": operation }),
        )?;
        journal.fail_run(run_id)?;
        Ok(())
    }
}

fn string_arg(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing string argument: {key}"))
}

fn payload_hash(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_string(value).unwrap_or_default().as_bytes());
    hex::encode(hasher.finalize())
}

fn has_mention(value: &Value) -> bool {
    value
        .get("mentions")
        .and_then(Value::as_array)
        .map(|mentions| !mentions.is_empty())
        .unwrap_or(false)
}

fn normalized_text(value: &Value) -> String {
    value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(500)
        .collect()
}
