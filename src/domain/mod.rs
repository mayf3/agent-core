use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use uuid::Uuid;

pub mod capability_change;
pub mod capability_proposal_link;
pub mod coding_operations;
pub mod context_block;
pub mod harness_change_request;
pub mod operation;
pub mod retry;
pub mod status;
pub use capability_proposal_link::*;
pub use context_block::*;
pub use harness_change_request::*;
pub use operation::*;
pub use retry::*;
pub use status::*;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalSource {
    Cli,
    Feishu,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub operation: String,
    pub scope: String,
}

/// The execution mode of a Run. Determines which operations, workspace, and
/// validation rules apply. Only the internal trusted constructor can set Hcr;
/// external creation paths always produce Default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// A normal user-initiated Run (CLI or Feishu chat). Standard grants and
    /// policy apply.
    Default,
    /// An HCR-bound Run, created by the internal worker after a successful
    /// atomic claim. The fields are loaded from the persisted claim record
    /// and are NOT accepted from external input.
    Hcr {
        /// The HarnessChangeRequest id this Run is bound to.
        hcr_id: String,
        /// The harness id the HCR is creating/changing.
        harness_id: String,
        /// The claim id that claimed this HCR for execution.
        claim_id: String,
    },
}

impl Default for RunMode {
    fn default() -> Self {
        RunMode::Default
    }
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
    /// The immutable registry snapshot this Run is pinned to. Context,
    /// Provider tools, and Gateway validation all read from this snapshot.
    /// Non-empty for all new Runs; old Runs are backfilled at boot.
    #[serde(default)]
    pub registry_snapshot_id: String,
    /// The execution mode of this Run. Default for ordinary Runs; Hcr for
    /// HCR-bound Runs. Modes carry mode-specific validation rules.
    /// External creation paths always produce Default.
    #[serde(default)]
    pub mode: RunMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunStatus {
    Running,
    WaitingDispatch,
    Completed,
    Failed,
    /// The run is paused awaiting a human approval decision (Phase 2 M2d).
    /// Set only when an operator opts in (`require_write_approval`) and a
    /// `risk: Write` operation has been proposed. Distinct from
    /// `WaitingDispatch` (not yet dispatched) and `Unknown` (dispatched with
    /// no terminal receipt): here the run is *intentionally* held until an
    /// `ApprovalGranted`/`ApprovalDenied` fact resumes it. Stored in
    /// `runs.status` as the raw string `"AwaitingApproval"`.
    AwaitingApproval,
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
    /// The chat type for Feishu events ("p2p" for private chat, "group" for
    /// group chat). None for non-Feishu events. Used for owner-scoped coding
    /// permission checks: only p2p from the configured owner gets coding grants.
    pub chat_type: Option<String>,
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

/// Typed errors produced by adapters so `DispatchErrorCategory` can classify
/// dispatch failures by variant instead of fragile string-substring matching.
/// Replaces the old `from_error(msg.contains("timeout")/...)` sniffing (Phase
/// 2 M2a typed-errors follow-up). Each variant maps 1:1 to a
/// `DispatchErrorCategory` in `DispatchErrorCategory::from_error`.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// Connect, read, or write timeout (the dispatch could not complete in the
    /// configured budget). Outcome is uncertain.
    #[error("adapter timeout")]
    Timeout,
    /// The connector returned a non-2xx HTTP status or otherwise signaled a
    /// definite execution failure. Outcome is known-bad.
    #[error("connector execute failed")]
    ExecuteFailed,
    /// The connector returned a malformed or unparseable response.
    #[error("adapter malformed response")]
    MalformedResponse,
    /// The approved invocation is missing a required argument.
    #[error("invalid approved invocation: {0}")]
    InvalidArgument(String),
    /// Any other transport/IO failure not covered above.
    #[error("adapter transport error: {0}")]
    Transport(String),
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
        // Prefer the typed `AdapterError` variant when the adapter produced one
        // (downcast). Fall back to `UnknownTransportError` for non-adapter
        // errors. This replaces the old string-substring sniffing
        // (`contains("timeout")` / `contains("connector execute failed")`).
        if let Some(adapter_error) = error.downcast_ref::<AdapterError>() {
            match adapter_error {
                AdapterError::Timeout => DispatchErrorCategory::AdapterTimeout,
                AdapterError::ExecuteFailed => DispatchErrorCategory::ConnectorExecuteFailed,
                AdapterError::MalformedResponse => DispatchErrorCategory::AdapterFailed,
                AdapterError::InvalidArgument(_) => {
                    DispatchErrorCategory::InvalidApprovedInvocation
                }
                AdapterError::Transport(_) => DispatchErrorCategory::UnknownTransportError,
            }
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
    /// Model emitted a valid or malformed tool call, before validation. The
    /// payload contains only bounded operation metadata and an internal id.
    ToolCallIssued,
    /// Model-emitted tool call was rejected during validation (unknown/write
    /// operation, malformed arguments, etc.). Written INSTEAD of
    /// InvocationProposed when the tool call does not pass validation.
    /// No ReceiptReceived corresponds to a ToolCallRejected — the invocation
    /// was never approved or executed.
    ToolCallRejected,
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
    /// Explicitly recorded when a reply invocation was successfully delivered
    /// to the user (stdout/feishu). The payload carries the final user-visible
    /// text so conversation history can be reconstructed without guessing from
    /// arbitrary connector output. Written atomically with the successful outbox
    /// dispatch transaction. Only operations in the reply white-list
    /// (stdout.send_text, feishu.send_message) produce this event.
    ///
    /// Payload fields:
    /// - session_id: &str
    /// - run_id: &str
    /// - invocation_id: &str (the reply invocation, "reply:<run_id>")
    /// - channel: "cli" | "feishu"
    /// - text: "final delivered text"
    AssistantReplyDelivered,
    WorkerJobDead,
    RunCompleted,
    RunFailed,
    // Phase 2 M2d: durable approval state. Appended when a `risk: Write`
    // operation is held for a human decision, and when that decision lands.
    ApprovalRequested,
    ApprovalGranted,
    ApprovalDenied,
    // Phase 2 M2d follow-up: an AwaitingApproval run whose approval TTL
    // elapsed (operator never decided). Terminal — the run fails. See
    // docs/decisions/m2d-durable-approval.md.
    ApprovalExpired,
    /// External harness manifest was registered (immutable content recorded).
    /// payload: `manifest_id`, `harness_id`, `artifact_digest`, `operation_name`, `protocol_version`
    /// correlation_id: manifest_id
    HarnessManifestRegistered,
    /// External hook was called (context.prepare.v0 or other hook kinds).
    /// payload: `hook`, `run_id`, `session_id`, `status`, `failure_mode`,
    ///          `fragment_count`, `resource_ref_count`, `response_bytes`, `duration_ms`, `error_code`
    /// correlation_id: run_id
    HookCallRecorded,
    /// Registry snapshot was activated (enable/disable took effect).
    /// payload: `action`, `manifest_id`, `operation_name`, `previous_snapshot_id`, `new_snapshot_id`, `decision_id`
    /// correlation_id: decision_id
    RegistrySnapshotActivated,
    // Capability Change Proposal lifecycle events.
    CapabilityChangeProposed,
    CapabilityChangeApproved,
    CapabilityChangeRejected,
    CapabilityChangeActivated,
    CapabilityChangeActivationFailed,
    CapabilityChangeExpired,
    /// External operation grant lifecycle events.
    /// payload: `grant_id`, `operation`, `grantee_principal_id`, `channel`,
    ///          `scope`, `risk`, `snapshot_id`
    ExternalOperationGranted,
    /// payload: `grant_id`, `operation`
    ExternalOperationRevoked,
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
    /// A HarnessChangeRequest was received, authorized, validated, and persisted.
    /// payload: `request_id`, `source`, `source_message_id`, `harness_id`, `requirement`,
    ///          `principal_id`, `channel`, `chat_type`, `session_id`, `status`
    /// correlation_id: request_id
    HarnessChangeRequested,
    /// An HCR was successfully claimed by a worker. The first stateful action.
    /// payload: `claim_id`, `hcr_id`, `harness_id`, `worker_instance_id`, `claimed_at`
    /// correlation_id: claim_id
    HcrClaimSucceeded,
    /// An HCR claim was rejected (already claimed, wrong status, etc.).
    /// payload: `hcr_id`, `reason`
    /// correlation_id: hcr_id
    HcrClaimRejected,
    /// An HCR-bound Run was created or resumed after a successful claim.
    /// payload: `claim_id`, `hcr_id`, `run_id`, `is_resume`
    /// correlation_id: claim_id
    HcrRunCreated,
    /// A durable gate evidence record was successfully registered.
    /// payload: `evidence_id`, `hcr_id`, `claim_id`, `run_id`, `gate_kind`, `receipt_id`
    /// correlation_id: evidence_id
    HcrEvidenceRegistered,
    /// HCR settlement succeeded: all gates passed structured validation.
    /// payload: `hcr_id`, `claim_id`, `run_id`, `result`, `evidence_set_digest`, `settlement_id`
    /// correlation_id: hcr_id
    HcrSettlementSucceeded,
    /// HCR settlement failed due to candidate code failure.
    /// payload: `hcr_id`, `claim_id`, `run_id`, `result`, `error_code`, `evidence_set_digest`, `settlement_id`
    /// correlation_id: hcr_id
    HcrSettlementFailed,
    /// The Run exhausted its configured tool-round budget. The Run completes
    /// normally so the reply is delivered, but the user must start a new Run
    /// to continue. payload: `run_id`, `tool_rounds_used`, `max_tool_rounds`
    ToolBudgetExhausted,
    /// The Run exceeded the wall-clock timeout for the tool recall loop.
    /// Written before the next LLM completion call when elapsed >=
    /// `config.tool_loop_timeout_ms`. payload: `run_id`, `elapsed_ms`,
    /// `timeout_ms`
    ToolLoopWallClockExceeded,
    /// The LLM emitted the same valid tool call (same operation + canonicalized
    /// arguments) consecutively, indicating a loop. Only applies to the coding
    /// harness mutating set. payload: `run_id`, `operation`, `turn_index`
    ToolLoopDetected,
}
