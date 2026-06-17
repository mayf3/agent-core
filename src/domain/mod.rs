use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use uuid::Uuid;

pub mod status;
pub mod retry;
pub mod operation;
pub use status::*;
pub use retry::*;
pub use operation::*;

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::new_v4().simple()))
            }
        }
    };
}

id_type!(AgentId, "agent");
id_type!(SessionId, "session");
id_type!(RunId, "run");
id_type!(EventId, "event");
id_type!(InvocationId, "invocation");
id_type!(PrincipalId, "principal");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub display_name: String,
    pub profile_path: PathBuf,
    pub skill_refs: Vec<SkillRef>,
    pub default_model: String,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRef {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentStatus {
    Active,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent_id: AgentId,
    pub channel: ChannelKind,
    pub conversation_key: String,
    pub summary: Option<String>,
    pub summarized_until_event_id: Option<EventId>,
    pub last_active_at: DateTime<Utc>,
    pub status: SessionStatus,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelKind {
    Cli,
    Feishu,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionStatus {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPrincipal {
    pub principal_id: PrincipalId,
    pub subject: PrincipalSubject,
    pub source: PrincipalSource,
    pub grants: Vec<CapabilityGrant>,
    pub requester_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PrincipalSubject {
    LocalUser,
    FeishuOpenId(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PrincipalSource {
    Cli,
    Feishu,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub operation: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub trigger_event_id: EventId,
    pub principal: RunPrincipal,
    pub parent_run_id: Option<RunId>,
    pub delegated_by: Option<PrincipalId>,
    pub status: RunStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunStatus {
    Running,
    WaitingDispatch,
    Completed,
    Failed,
    /// The dispatch outcome is unknown: the run was dispatched but no
    /// terminal receipt was ever recorded, so its result cannot be
    /// determined. Recovery sets this when an outbox row is reconciled to
    /// `unknown` (see `recover_unknown_invocations`). Distinct from
    /// `WaitingDispatch`, which means "not yet dispatched". Stored in
    /// `runs.status` as the raw string `"Unknown"` (no exhaustive match
    /// reads it back; see `docs/decisions/runstatus-unknown.md`).
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressEnvelope {
    pub protocol_version: String,
    pub source: ExternalSource,
    pub external_event_id: String,
    pub received_at: DateTime<Utc>,
    pub payload: Value,
    pub auth_context: AuthContext,
    pub routing_hint: Option<RoutingHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExternalSource {
    Cli,
    Feishu,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    pub authenticated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingHint {
    pub agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedEvent {
    pub event_id: EventId,
    pub source: EventSource,
    pub principal: RunPrincipal,
    pub session_target: SessionTarget,
    pub payload: RuntimeEventPayload,
    pub dedupe_key: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventSource {
    Cli,
    Feishu,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTarget {
    pub agent_id: AgentId,
    pub channel: ChannelKind,
    pub conversation_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuntimeEventPayload {
    UserMessage {
        text: String,
        message_id: Option<String>,
        chat_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlock {
    pub kind: ContextBlockKind,
    pub content: String,
    pub compressibility: Compressibility,
    pub source_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContextBlockKind {
    RootSystem,
    RuntimeContract,
    AgentProfile,
    SkillCatalog,
    ActiveSkill,
    RecentMessages,
    UserMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Compressibility {
    Never,
    DropWhole,
    Summarizable,
    Truncate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationIntent {
    pub invocation_id: InvocationId,
    pub run_id: RunId,
    pub operation: String,
    pub arguments: Value,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedInvocation {
    intent: InvocationIntent,
    pub decision_id: String,
}

impl ApprovedInvocation {
    pub(crate) fn new(intent: InvocationIntent, decision_id: String) -> Self {
        Self {
            intent,
            decision_id,
        }
    }

    pub fn intent(&self) -> &InvocationIntent {
        &self.intent
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub invocation_id: InvocationId,
    pub status: ReceiptStatus,
    pub external_ref: Option<String>,
    pub output: Value,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptStatus {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchErrorCategory {
    AdapterTimeout,
    ConnectorExecuteFailed,
    AdapterFailed,
    InvalidApprovedInvocation,
    UnsupportedOperation,
    UnknownTransportError,
}

impl DispatchErrorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            DispatchErrorCategory::AdapterTimeout => "adapter_timeout",
            DispatchErrorCategory::ConnectorExecuteFailed => "connector_execute_failed",
            DispatchErrorCategory::AdapterFailed => "adapter_failed",
            DispatchErrorCategory::InvalidApprovedInvocation => "invalid_approved_invocation",
            DispatchErrorCategory::UnsupportedOperation => "unsupported_operation",
            DispatchErrorCategory::UnknownTransportError => "unknown_transport_error",
        }
    }

    pub fn from_error(error: &anyhow::Error) -> Self {
        let msg = error.to_string().to_ascii_lowercase();
        if msg.contains("timeout") {
            DispatchErrorCategory::AdapterTimeout
        } else if msg.contains("connector execute failed") {
            DispatchErrorCategory::ConnectorExecuteFailed
        } else if msg.contains("adapter") || msg.contains("execute") {
            DispatchErrorCategory::AdapterFailed
        } else {
            DispatchErrorCategory::UnknownTransportError
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    pub sequence: i64,
    pub event_id: EventId,
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub correlation_id: Option<String>,
    pub kind: JournalEventKind,
    pub payload: Value,
    pub previous_hash: Option<String>,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeasedOutboxDispatch {
    pub invocation_id: InvocationId,
    pub run_id: RunId,
    pub session_id: Option<SessionId>,
    pub operation: String,
    pub arguments: Value,
    pub idempotency_key: String,
    pub decision_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownInvocation {
    pub invocation_id: String,
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub first_dispatch_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JournalEventKind {
    IngressAccepted,
    SessionReady,
    RunStarted,
    ContextBuilt,
    LlmCompleted,
    InvocationProposed,
    InvocationApproved,
    WorkerJobQueued,
    WorkerJobStarted,
    WorkerJobSucceeded,
    WorkerJobFailed,
    OutboxQueued,
    OutboxDispatchFailed,
    OutboxDispatchUnknown,
    OutboxDispatchDead,
    DispatchStarted,
    ReceiptReceived,
    WorkerJobDead,
    RunCompleted,
    RunFailed,
    /// Sentinel produced by `parse_kind`/`row_to_event` when the stored
    /// `kind` text does not match any known variant. The kernel never writes
    /// `Unknown` — observing it at read time indicates either external
    /// tampering or a future enum variant whose read-path wasn't updated.
    /// Routing unknown kinds here (rather than silently to `RunCompleted`)
    /// keeps them from masquerading as a run completion in the recovery
    /// predicates (`undelivered_ingress_events`); `verify_hash_chain` still
    /// flags the row as corrupt since the re-serialized string won't match.
    /// See HANDOVER §10.
    Unknown,
}
