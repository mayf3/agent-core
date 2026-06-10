use crate::config::KernelConfig;
use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

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
        let has_grant = run
            .principal
            .grants
            .iter()
            .any(|grant| grant.operation == intent.operation);
        if !has_grant {
            bail!("capability_not_enabled: {}", intent.operation);
        }
        if intent.operation != "stdout.send_text" && intent.operation != "feishu.send_message" {
            bail!("operation_not_allowed: {}", intent.operation);
        }
        let target_session = string_arg(&intent.arguments, "session_id")?;
        if target_session != session.id.0 {
            bail!("target_session_mismatch");
        }
        if intent.operation == "feishu.send_message" {
            string_arg(&intent.arguments, "message_id")?;
            string_arg(&intent.arguments, "chat_id")?;
            string_arg(&intent.arguments, "text")?;
        }
        Ok(ApprovedInvocation::new(
            intent,
            format!("decision_{}", Uuid::new_v4().simple()),
        ))
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
                grants: vec![CapabilityGrant {
                    operation: "stdout.send_text".to_string(),
                    scope: "current_session".to_string(),
                }],
                requester_id: Some("cli:local".to_string()),
            },
            session_target: SessionTarget {
                agent_id: self.config.agent_id.clone(),
                channel: ChannelKind::Cli,
                conversation_key: "local".to_string(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text,
                message_id: None,
                chat_id: None,
            },
            dedupe_key: format!("{source}:{}", envelope.external_event_id),
            occurred_at: envelope.received_at,
        };
        journal.append_event(
            JournalEventKind::IngressAccepted,
            None,
            None,
            Some(&event.dedupe_key),
            json!({
                "source": source,
                "external_event_id": envelope.external_event_id,
                "event_id": event_id.0,
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
        if !journal.reserve_ingress("feishu", &envelope.external_event_id, &event_id)? {
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
                grants: vec![CapabilityGrant {
                    operation: "feishu.send_message".to_string(),
                    scope: "current_session".to_string(),
                }],
                requester_id: Some(format!("feishu:open_id:{sender_open_id}")),
            },
            session_target: SessionTarget {
                agent_id: self.config.agent_id.clone(),
                channel: ChannelKind::Feishu,
                conversation_key,
            },
            payload: RuntimeEventPayload::UserMessage {
                text,
                message_id: Some(message_id.clone()),
                chat_id: Some(chat_id.clone()),
            },
            dedupe_key: format!("feishu:{}", envelope.external_event_id),
            occurred_at: envelope.received_at,
        };
        journal.append_event(
            JournalEventKind::IngressAccepted,
            None,
            None,
            Some(&event.dedupe_key),
            json!({
                "source": "feishu",
                "external_event_id": envelope.external_event_id,
                "event_id": event_id.0,
                "sender_open_id": sender_open_id,
                "chat_id": chat_id,
                "message_id": message_id,
                "message_type": message_type,
                "text": normalized_text(&envelope.payload),
                "payload_hash": payload_hash(&envelope.payload),
            }),
        )?;
        Ok(event)
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
