use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

mod common;

use agent_core_kernel::domain::operation::FEISHU_SEND_MESSAGE;
use agent_core_kernel::gateway::Gateway;

#[test]
fn gateway_cli_ingress_grants_configured_extra_operations() -> Result<()> {
    // Phase 2 M2b config-driven half: when the operator configures extra
    // catalog operations, a CLI ingress principal receives them in addition
    // to its channel baseline grant. The principal may then be approved for
    // those operations (the gateway allowlist is the catalog).
    let mut config = common::test_config();
    config.extra_allowed_operations = vec![FEISHU_SEND_MESSAGE.to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let event = gateway.validate_ingress(&journal, gateway.cli_ingress("hi".to_string())?)?;
    let operations: Vec<&str> = event
        .principal
        .grants
        .iter()
        .map(|g| g.operation.as_str())
        .collect();
    assert!(
        operations.contains(&"stdout.send_text"),
        "baseline cli grant kept"
    );
    assert!(
        operations.contains(&FEISHU_SEND_MESSAGE),
        "configured extra operation granted"
    );
    Ok(())
}

#[test]
fn gateway_cli_ingress_drops_uncatalogued_extra_operations() -> Result<()> {
    // Operations not in the catalog can never be approved (the gateway
    // allowlist is the catalog), so they must not appear as grants even when
    // an operator mistakenly lists them.
    let mut config = common::test_config();
    config.extra_allowed_operations = vec!["shell.exec".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let event = gateway.validate_ingress(&journal, gateway.cli_ingress("hi".to_string())?)?;
    let operations: Vec<&str> = event
        .principal
        .grants
        .iter()
        .map(|g| g.operation.as_str())
        .collect();
    assert_eq!(
        operations,
        vec!["stdout.send_text", "session.recall_recent"]
    );
    Ok(())
}

#[test]
fn stale_running_worker_job_is_re_leased_on_next_poll() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_re_lease".to_string());
    let job_id = journal.enqueue_worker_job(&event_id)?;
    let first = journal.lease_next_worker_job()?;
    assert_eq!(first.as_ref().map(|e| &e.0), Some(&event_id.0));
    let second = journal.lease_next_worker_job()?;
    assert!(second.is_none());
    journal.expire_worker_lease_for_test(&job_id)?;
    let re_leased = journal.lease_next_worker_job()?;
    assert_eq!(re_leased.as_ref().map(|e| &e.0), Some(&event_id.0));
    assert_eq!(journal.worker_job_stale_count()?, 0);
    Ok(())
}

use agent_core_kernel::registry::snapshot::test_snapshot;
use agent_core_kernel::runtime::outbox_dispatcher::dispatch_once;
use agent_core_kernel::context::ContextAssembler;
use common::FakeReplyAdapter;

fn lt(j: &JournalStore, sid: &SessionId, ev: &str, ut: &str, rid: &str, rt: &str) -> Result<()> {
    j.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some(ev),
        json!({"source":"feishu","event_id":ev,"text":ut}),
    )?;
    j.append_event(
        JournalEventKind::SessionReady,
        None,
        Some(sid),
        Some(ev),
        json!({"session_id":sid.0}),
    )?;
    let r = RunId(rid.into());
    let c = format!("reply:{rid}");
    j.append_event(
        JournalEventKind::RunStarted,
        Some(&r),
        Some(sid),
        Some(ev),
        json!({"run_id":rid}),
    )?;
    j.append_event(
        JournalEventKind::AssistantReplyDelivered,
        Some(&r),
        Some(sid),
        Some(&c),
        json!({"session_id":sid.0,"run_id":rid,"invocation_id":c,"channel":"cli","text":rt}),
    )?;
    Ok(())
}
fn lu(j: &JournalStore, sid: &SessionId, ev: &str, ut: &str, rid: &str) -> Result<()> {
    j.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some(ev),
        json!({"source":"feishu","event_id":ev,"text":ut}),
    )?;
    j.append_event(
        JournalEventKind::SessionReady,
        None,
        Some(sid),
        Some(ev),
        json!({"session_id":sid.0}),
    )?;
    let r = RunId(rid.into());
    j.append_event(
        JournalEventKind::RunStarted,
        Some(&r),
        Some(sid),
        Some(ev),
        json!({"run_id":rid}),
    )?;
    Ok(())
}
fn lr(j: &JournalStore, sid: &SessionId, rid: &str, rt: &str) -> Result<()> {
    let r = RunId(rid.into());
    let c = format!("reply:{rid}");
    j.append_event(
        JournalEventKind::AssistantReplyDelivered,
        Some(&r),
        Some(sid),
        Some(&c),
        json!({"session_id":sid.0,"run_id":rid,"invocation_id":c,"channel":"cli","text":rt}),
    )?;
    Ok(())
}
fn lrp(j: &JournalStore, sid: &SessionId, rid: &str, pv: Value) -> Result<()> {
    let r = RunId(rid.into());
    let c = format!("reply:{rid}");
    j.append_event(
        JournalEventKind::AssistantReplyDelivered,
        Some(&r),
        Some(sid),
        Some(&c),
        pv,
    )?;
    Ok(())
}
fn apv(j: &JournalStore, g: &Gateway, rid: &RunId, sid: &SessionId) -> Result<ApprovedInvocation> {
    let snap = test_snapshot();
    let run = common::runtime_run(rid, sid);
    j.insert_run(&run)?;
    let sess = Session {
        id: sid.clone(),
        channel: ChannelKind::Cli,
        ..common::test_session(&common::test_config())
    };
    let ap = g.approve_invocation(
        InvocationIntent {
            invocation_id: InvocationId(format!("reply:{}", rid.0)),
            run_id: rid.clone(),
            operation: "stdout.send_text".into(),
            arguments: json!({"text":"hello","session_id":sid.0}),
            idempotency_key: None,
        },
        &run,
        &sess,
        &snap,
    )?;
    j.queue_outbox_dispatch(&ap, Some(sid))?;
    Ok(ap)
}
fn rcp(inv: &str) -> Receipt {
    Receipt {
        invocation_id: InvocationId(inv.into()),
        status: ReceiptStatus::Succeeded,
        output: json!({"status":"sent"}),
        external_ref: None,
        occurred_at: Utc::now(),
    }
}

#[test]
fn conversation_turns_reject_mismatched_assistant_event_identity() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "user", "r1", "valid")?;
    lu(&j, &s, "e2", "u2", "r2")?;
    lrp(
        &j,
        &s,
        "r2",
        json!({"session_id":"wrong","run_id":"r2","invocation_id":"reply:r2","channel":"cli","text":"bad1"}),
    )?;
    lu(&j, &s, "e3", "u3", "r3")?;
    let _r3 = RunId("r3".into());
    j.append_event(JournalEventKind::AssistantReplyDelivered,None,Some(&s),Some("reply:r3"),json!({"session_id":s.0,"run_id":"r3","invocation_id":"reply:r3","channel":"cli","text":"bad2"}))?;
    lu(&j, &s, "e4", "u4", "r4")?;
    lrp(
        &j,
        &s,
        "r4",
        json!({"session_id":s.0,"run_id":"r4","invocation_id":"","channel":"cli","text":"bad3"}),
    )?;
    lu(&j, &s, "e5", "u5", "r5")?;
    let r5 = RunId("r5".into());
	j.append_event(JournalEventKind::AssistantReplyDelivered,Some(&r5),Some(&s),Some("wrong_corr"),json!({"session_id":s.0,"run_id":"r5","invocation_id":"reply:r5","channel":"cli","text":"bad4"}))?;
	// event.session_id缺失
	lu(&j, &s, "e6", "u6", "r6")?;
	let r6 = RunId("r6".into());
	j.append_event(JournalEventKind::AssistantReplyDelivered,Some(&r6),None,Some("reply:r6"),json!({"session_id":s.0,"run_id":"r6","invocation_id":"reply:r6","channel":"cli","text":"bad5"}))?;
	// event.run_id与payload.run_id不一致
	lu(&j, &s, "e7", "u7", "r7")?;
	let r7 = RunId("r7".into());
	j.append_event(JournalEventKind::AssistantReplyDelivered,Some(&r7),Some(&s),Some("reply:r7"),json!({"session_id":s.0,"run_id":"r7_wrong","invocation_id":"reply:r7","channel":"cli","text":"bad6"}))?;
	// event.correlation_id缺失
	lu(&j, &s, "e8", "u8", "r8")?;
	let r8 = RunId("r8".into());
	j.append_event(JournalEventKind::AssistantReplyDelivered,Some(&r8),Some(&s),None,json!({"session_id":s.0,"run_id":"r8","invocation_id":"reply:r8","channel":"cli","text":"bad7"}))?;
	let t = j.recent_conversation_turns(&s, 10, None)?;
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].0, "user");
    assert_eq!(t[0].1, "valid");
    Ok(())
}
#[test]
fn conversation_turns_pair_by_run_id_when_replies_complete_out_of_order() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lu(&j, &s, "ea", "user A", "rA")?;
    lu(&j, &s, "eb", "user B", "rB")?;
    lr(&j, &s, "rB", "reply B")?;
    lr(&j, &s, "rA", "reply A")?;
    let t = j.recent_conversation_turns(&s, 10, None)?;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0, "user A");
    assert_eq!(t[0].1, "reply A");
    assert_eq!(t[1].0, "user B");
    assert_eq!(t[1].1, "reply B");
    Ok(())
}
#[test]
fn conversation_turn_limit_one_keeps_latest_complete_turn() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "u1", "r1", "r1")?;
    lt(&j, &s, "e2", "u2", "r2", "r2")?;
    assert_eq!(j.recent_conversation_turns(&s, 1, None)?[0].0, "u2");
    Ok(())
}
#[test]
fn conversation_turn_limit_overflow_keeps_latest_complete_turns() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    for i in 0..4 {
        lt(
            &j,
            &s,
            &format!("e{i}"),
            &format!("u{i}"),
            &format!("r{i}"),
            "r",
        )?;
    }
    let t = j.recent_conversation_turns(&s, 2, None)?;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0, "u2");
    assert_eq!(t[1].0, "u3");
    Ok(())
}
#[test]
fn incomplete_turn_does_not_consume_limit() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "complete", "r1", "reply")?;
    lu(&j, &s, "e2", "incomplete", "r2")?;
    assert_eq!(j.recent_conversation_turns(&s, 1, None)?[0].0, "complete");
    Ok(())
}
#[test]
fn failed_turn_does_not_consume_limit() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "user A", "r1", "reply A")?;
    lu(&j, &s, "e2", "user B (failed)", "r2")?;
    j.append_event(
        JournalEventKind::RunFailed,
        Some(&RunId("r2".into())),
        Some(&s),
        Some("e2"),
        json!({"status":"Failed"}),
    )?;
    lt(&j, &s, "e3", "user C", "r3", "reply C")?;
    let t = j.recent_conversation_turns(&s, 2, None)?;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0, "user A");
    assert_eq!(t[1].0, "user C");
    Ok(())
}
#[test]
fn current_run_is_excluded_from_recent_turns() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "ev_p", "prior", "r_p", "reply")?;
    lt(&j, &s, "ev_c", "current", "r_c", "rep")?;
    assert_eq!(
        j.recent_conversation_turns(&s, 10, Some("ev_c"))?[0].0,
        "prior"
    );
    Ok(())
}
#[test]
fn connector_unknown_fields_are_not_persisted_in_journal() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(common::test_config());
    let sid = SessionId("sx".into());
    let rid = RunId("rx".into());
    j.append_event(
        JournalEventKind::IngressAccepted,
        None,
        None,
        Some("ev_sx"),
        json!({"source":"feishu","event_id":"ev_sx","text":"安全测试用户消息"}),
    )?;
    j.append_event(
        JournalEventKind::SessionReady,
        None,
        Some(&sid),
        Some("ev_sx"),
        json!({"session_id":sid.0}),
    )?;
    j.append_event(
        JournalEventKind::RunStarted,
        Some(&rid),
        Some(&sid),
        Some("ev_sx"),
        json!({"run_id":rid.0}),
    )?;
    apv(&j, &g, &rid, &sid)?;
    assert!(
        dispatch_once(
            &j,
            &FakeReplyAdapter {
                receipt: Receipt {
                    invocation_id: InvocationId("reply:rx".into()),
                    status: ReceiptStatus::Succeeded,
                    output: json!({"status":"sent","SECRET_TOKEN_MARKER":"x","/private/internal/path":"y","nested":{"secret":"NESTED_SECRET_MARKER"},"large_unknown":"LARGE_UNKNOWN_MARKER.."}),
                    external_ref: None,
                    occurred_at: Utc::now()
                }
            }
        )?,
        "dispatch must succeed"
    );
    let ev = j.events()?;
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        1
    );
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .count(),
        1
    );
    assert_eq!(
        j.outbox_dispatch_status(&InvocationId("reply:rx".into()))?,
        Some(OutboxDispatchStatus::Succeeded)
    );
    let body = serde_json::to_string(&ev).unwrap_or_default();
    for m in &[
        "SECRET_TOKEN_MARKER",
        "/private/internal/path",
        "NESTED_SECRET_MARKER",
        "LARGE_UNKNOWN_MARKER",
    ] {
        assert!(!body.contains(m), "leaked {m}");
    }
    let turns = j.recent_conversation_turns(&sid, 10, None)?;
    assert_eq!(turns.len(), 1, "must have 1 complete turn");
    assert_eq!(turns[0].0, "安全测试用户消息", "user text");
    assert_eq!(turns[0].1, "hello", "assistant from args");
    for m in &[
        "SECRET_TOKEN_MARKER",
        "/private/internal/path",
        "NESTED_SECRET_MARKER",
        "LARGE_UNKNOWN_MARKER",
    ] {
	assert!(!format!("{:?}", turns).contains(m), "leaked in turns");
	}
	// ── ContextAssembler real proof ──
	let config = common::test_config();
	let ca = ContextAssembler::from_config(&config);
	let sess = Session {
	    id: sid.clone(),
	    agent_id: config.agent_id.clone(),
	    channel: ChannelKind::Feishu,
	    conversation_key: "local".to_string(),
	    summary: None,
	    summarized_until_event_id: None,
	    last_active_at: Utc::now(),
	    status: SessionStatus::Active,
	    version: 1,
	};
	let ingress_event = ValidatedEvent {
	    event_id: EventId("__none__".into()),
	    source: EventSource::Feishu,
	    principal: common::cli_principal(),
	    session_target: SessionTarget {
		agent_id: config.agent_id.clone(),
		channel: ChannelKind::Feishu,
		conversation_key: "local".to_string(),
	    },
	    payload: RuntimeEventPayload::UserMessage {
		text: "安全测试用户消息".into(),
		message_id: None,
		chat_id: None,
	    },
	    dedupe_key: "__none__".into(),
	    occurred_at: Utc::now(),
	};
	let granted_ops: Vec<String> = common::cli_principal()
	    .grants
	    .iter()
	    .map(|g| g.operation.clone())
	    .collect();
	let snap = test_snapshot();
	let blocks = ca.build(&j, &sess, &ingress_event, "安全测试用户消息", &granted_ops, &snap)?;
	let recent = blocks
	    .iter()
		.find(|b| matches!(b.kind, ContextBlockKind::RecentMessages))
	    .expect("must have RecentMessages ContextBlock");
	assert!(
	    recent.content.contains("User: 安全测试用户消息"),
	    "user text in RecentMessages"
	);
	assert!(
	    recent.content.contains("Assistant: hello"),
	    "assistant text in RecentMessages"
	);
	for m in &[
	    "SECRET_TOKEN_MARKER",
	    "/private/internal/path",
	    "NESTED_SECRET_MARKER",
	    "LARGE_UNKNOWN_MARKER",
	] {
	    assert!(!recent.content.contains(m), "leaked {m} in ContextBlock");
	}
	Ok(())
}
#[test]
fn assistant_reply_delivered_transaction_failure_rolls_back_all() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(common::test_config());
    let sid = SessionId("t".into());
    apv(&j, &g, &RunId("rt".into()), &sid)?;
    j.execute_sql_for_test("CREATE TRIGGER fail_ard BEFORE INSERT ON journal_events WHEN NEW.kind='AssistantReplyDelivered' BEGIN SELECT RAISE(ABORT,'forced'); END")?;
    assert!(
        dispatch_once(
            &j,
            &FakeReplyAdapter {
                receipt: rcp("reply:rt")
            }
        )
        .is_err(),
        "must fail"
    );
    let ev = j.events()?;
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        0
    );
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .count(),
        0
    );
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::RunCompleted)
            .count(),
        0
    );
    assert_eq!(
        j.outbox_dispatch_status(&InvocationId("reply:rt".into()))?,
        Some(OutboxDispatchStatus::Dispatching)
	    );
	
	let run = j.run(&RunId("rt".into()))?.expect("run must exist");
	assert!(matches!(run.status, RunStatus::Running));

	Ok(())
}

#[test]
fn conversation_turn_limit_zero_returns_empty() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "u1", "r1", "r1")?;
    lt(&j, &s, "e2", "u2", "r2", "r2")?;
    assert!(
        j.recent_conversation_turns(&s, 0, None)?.is_empty(),
        "limit=0 must return empty"
    );
    Ok(())
}

#[test]
fn conversation_turn_limit_two_preserves_order() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "user A", "r1", "reply A")?;
    lt(&j, &s, "e2", "user B", "r2", "reply B")?;
    lt(&j, &s, "e3", "user C", "r3", "reply C")?;
    let t = j.recent_conversation_turns(&s, 2, None)?;
    assert_eq!(t.len(), 2, "limit=2 returns exactly 2");
    assert_eq!(t[0].0, "user B");
    assert_eq!(t[0].1, "reply B");
    assert_eq!(t[1].0, "user C");
    assert_eq!(t[1].1, "reply C");
    Ok(())
}
