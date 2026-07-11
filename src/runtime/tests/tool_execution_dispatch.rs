use crate::domain::{
    AgentId, ApprovedInvocation, CapabilityGrant, ChannelKind, EventId, InvocationId,
    InvocationIntent, PrincipalId, PrincipalSource, PrincipalSubject, Run, RunId, RunMode,
    RunPrincipal, RunStatus, Session, SessionId, SessionStatus,
};
use crate::journal::JournalStore;
use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use crate::runtime::tool_execution::dispatch_builtin_binding;
use crate::runtime::tool_loop::ToolCallOutcome;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
#[test]
fn retired_builtin_time_binding_returns_fail_closed_error() {
    let dir = std::env::temp_dir().join(format!("retired_time_dispatch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("kernel.sqlite");
    let journal = Arc::new(JournalStore::open(&db_path).expect("open"));
    let _ = journal.initialize_registry().expect("init");
    let spec = OperationSpec {
        name: "time.now".into(),
        risk: Risk::ReadOnly,
        description: "retired".into(),
        parameters: json!({"type": "object"}),
        idempotent: true,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.time_now".into(),
    };
    let run = Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        agent_id: AgentId("test".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("cli:test".into()),
            requester_id: None,
            subject: PrincipalSubject::LocalUser,
            source: PrincipalSource::Cli,
            grants: vec![CapabilityGrant {
                operation: "time.now".into(),
                scope: "current_session".to_string(),
            }],
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: "snap_legacy".into(),
        mode: RunMode::Default,
    };
    let session = Session {
        id: SessionId::new(),
        agent_id: AgentId("test".into()),
        channel: ChannelKind::Cli,
        conversation_key: "test".into(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    };
    let approved = ApprovedInvocation::new(
        InvocationIntent {
            invocation_id: InvocationId("test:retired:time".into()),
            run_id: run.id.clone(),
            operation: "time.now".into(),
            arguments: json!({}),
            idempotency_key: Some("test:retired:time".into()),
        },
        "decision:retired".into(),
    );
    let legacy_snapshot_id = run.registry_snapshot_id.clone();
    let outcome = dispatch_builtin_binding(
        &spec,
        &approved,
        &journal,
        &run,
        &session,
        "test:retired:time",
        std::time::Duration::from_secs(1),
        &run.registry_snapshot_id,
    );
    assert_eq!(
        run.registry_snapshot_id, legacy_snapshot_id,
        "Run registry_snapshot_id unchanged"
    );
    match outcome {
        ToolCallOutcome::ToolResult { text } => {
            assert!(
                text.contains("retired_builtin_operation"),
                "must contain retired_builtin_operation: {text}"
            );
            assert!(!text.contains("succeeded"), "must NOT succeed");
            assert!(!text.contains("iso"), "must NOT contain iso");
            assert!(!text.contains("epoch_ms"), "must NOT contain epoch_ms");
            assert!(!text.contains("external.time_now"), "must NOT invoke ext");
        }
        _ => panic!("expected ToolResult"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
