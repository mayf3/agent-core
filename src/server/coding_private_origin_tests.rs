use super::*;
use chrono::Utc;

fn private_session() -> Session {
    Session {
        id: SessionId("session_private".into()),
        agent_id: AgentId("main".into()),
        channel: ChannelKind::Feishu,
        conversation_key: "feishu:open_id:owner".into(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    }
}

fn owner_run(session: &Session) -> Run {
    let now = Utc::now();
    Run {
        id: RunId("run_private".into()),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: EventId("event_private".into()),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:owner".into()),
            subject: PrincipalSubject::FeishuOpenId("owner".into()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: Some("feishu:open_id:owner".into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: now,
        updated_at: now,
        registry_snapshot_id: "snap_test".into(),
        mode: RunMode::Default,
    }
}

fn ingress(source: EventSource, chat_type: Option<&str>) -> ValidatedEvent {
    ValidatedEvent {
        event_id: EventId("event_private".into()),
        source,
        principal: owner_run(&private_session()).principal,
        session_target: SessionTarget {
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:owner".into(),
        },
        payload: RuntimeEventPayload::UserMessage {
            text: "开发一个 external.calculator，支持加减乘除".into(),
            message_id: Some("om_1".into()),
            chat_id: Some("oc_1".into()),
        },
        dedupe_key: "feishu:message:om_1".into(),
        occurred_at: Utc::now(),
        chat_type: chat_type.map(str::to_string),
    }
}

#[test]
fn owner_private_context_is_the_only_valid_coding_origin() {
    let private = private_session();
    let owner = owner_run(&private);
    assert!(validate_private_owner_context(Some("owner"), &owner, &private).is_ok());

    let mut group = private.clone();
    group.conversation_key = "feishu:chat_id:group".into();
    assert!(validate_private_owner_context(Some("owner"), &owner, &group).is_err());

    let mut stranger = owner.clone();
    stranger.principal.principal_id = PrincipalId("feishu:open_id:stranger".into());
    stranger.principal.subject = PrincipalSubject::FeishuOpenId("stranger".into());
    assert!(validate_private_owner_context(Some("owner"), &stranger, &private).is_err());
    assert!(validate_private_owner_context(None, &owner, &private).is_err());
}

#[test]
fn coding_delivery_routes_only_feishu_p2p_events() {
    assert!(crate::server::coding_delivery::matches(&ingress(
        EventSource::Feishu,
        Some("p2p")
    )));
    assert!(!crate::server::coding_delivery::matches(&ingress(
        EventSource::Feishu,
        Some("group")
    )));
    assert!(!crate::server::coding_delivery::matches(&ingress(
        EventSource::Cli,
        None
    )));
}
