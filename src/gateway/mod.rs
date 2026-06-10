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
            ExternalSource::Feishu => bail!("feishu ingress is not enabled in M0"),
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
        if intent.operation != "stdout.send_text" {
            bail!("operation_not_allowed: {}", intent.operation);
        }
        let target_session = string_arg(&intent.arguments, "session_id")?;
        if target_session != session.id.0 {
            bail!("target_session_mismatch");
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
