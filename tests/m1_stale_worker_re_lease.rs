use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use serde_json::json;

/// Phase 1: a worker job whose lease expired (worker crashed mid-job) is
/// re-leased by `lease_next_worker_job` on the next poll. This is the
/// self-heal behavior the operating guide claims. Lock it down as a
/// regression test so a future refactor cannot silently break it.
#[test]
fn stale_running_worker_job_is_re_leased_on_next_poll() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_re_lease".to_string());
    let job_id = journal.enqueue_worker_job(&event_id)?;

    // First lease: job goes running with a live lease.
    let first = journal.lease_next_worker_job()?;
    assert_eq!(first.as_ref().map(|e| &e.0), Some(&event_id.0));

    // A second lease while the first is still live returns None (still owned).
    let second = journal.lease_next_worker_job()?;
    assert!(second.is_none(), "a live lease must not be re-acquired");

    // Simulate the worker crashing mid-job: expire the lease.
    journal.expire_worker_lease_for_test(&job_id)?;

    // The next poll must re-lease the stale job (self-heal).
    let re_leased = journal.lease_next_worker_job()?;
    assert_eq!(
        re_leased.as_ref().map(|e| &e.0),
        Some(&event_id.0),
        "a stale (expired-lease) running worker job must be re-leased"
    );

    // And the stale count is now 0 again (the job has a fresh live lease).
    assert_eq!(
        journal.worker_job_stale_count()?,
        0,
        "after re-leasing, the job is no longer stale"
    );
    Ok(())
}

// ===== Conversation continuity tests =====

use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::registry::snapshot::test_snapshot;
use chrono::Utc;
use serde_json::Value;

mod common;

fn setup_stdout_approval(journal: &JournalStore) -> Result<(Run, Session, ApprovedInvocation)> {
    let config = common::test_config();
    let gateway = Gateway::new(config);
    let session = common::test_session(&common::test_config());
    let run = common::runtime_run(&RunId("run_s".into()), &session.id);
    journal.insert_run(&run)?;
    let approved = common::approved_stdout_invocation(&gateway, &run, &session)?;
    Ok((run, session, approved))
}

fn setup_feishu_approval(
    journal: &JournalStore,
    sid_str: &str,
    rid_str: &str,
) -> Result<(Gateway, Run, Session, ApprovedInvocation)> {
    let config = common::test_config();
    let gateway = Gateway::new(config);
    let session = Session {
        id: SessionId(sid_str.into()),
        channel: ChannelKind::Feishu,
        ..common::test_session(&common::test_config())
    };
    let run = Run {
        id: RunId(rid_str.into()),
        session_id: session.id.clone(),
        agent_id: AgentId("main".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:user".into()),
            subject: PrincipalSubject::FeishuOpenId("user".into()),
            source: PrincipalSource::Feishu,
            grants: vec![CapabilityGrant {
                operation: "feishu.send_message".to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("feishu:user".into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: String::new(),
    };
    journal.insert_run(&run)?;
    let snap = test_snapshot();
    let approved = gateway.approve_invocation(InvocationIntent { invocation_id: InvocationId(format!("reply:{rid_str}")), run_id: run.id.clone(), operation: "feishu.send_message".to_string(), arguments: json!({"text": "feishu text", "session_id": session.id.0, "message_id": "m1", "chat_id": "oc1"}), idempotency_key: None }, &run, &session, &snap)?;
    Ok((gateway, run, session, approved))
}

fn queue_start_succeed(
    journal: &JournalStore,
    approved: &ApprovedInvocation,
    run: &Run,
    session: &Session,
) -> Result<()> {
    journal.queue_outbox_dispatch(approved, Some(&session.id))?;
    journal.start_outbox_dispatch(approved, Some(&session.id))?;
    journal.succeed_outbox_dispatch(
        &Receipt {
            invocation_id: approved.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            output: json!({"status": "sent"}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
        &run.id,
        Some(&session.id),
    )?;
    Ok(())
}

#[test]
fn successful_stdout_reply_records_assistant_reply_delivered() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (run, session, approved) = setup_stdout_approval(&journal)?;
    queue_start_succeed(&journal, &approved, &run, &session)?;
    let events = journal.events()?;
    let d: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
        .collect();
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].payload.get("text").and_then(Value::as_str),
        Some("hello")
    );
    assert_eq!(
        d[0].payload.get("channel").and_then(Value::as_str),
        Some("cli")
    );
    Ok(())
}

#[test]
fn successful_feishu_reply_records_assistant_reply_delivered() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (_, run, session, approved) = setup_feishu_approval(&journal, "sf", "rf")?;
    queue_start_succeed(&journal, &approved, &run, &session)?;
    let events = journal.events()?;
    let d: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
        .collect();
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].payload.get("text").and_then(Value::as_str),
        Some("feishu text")
    );
    assert_eq!(
        d[0].payload.get("channel").and_then(Value::as_str),
        Some("feishu")
    );
    Ok(())
}

#[test]
fn failed_stdout_reply_does_not_record_assistant_reply_delivered() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (run, session, approved) = setup_stdout_approval(&journal)?;
    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    journal.fail_outbox_dispatch(
        &approved.intent().invocation_id,
        &run.id,
        Some(&session.id),
        "failed",
    )?;
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        0
    );
    Ok(())
}

#[test]
fn failed_feishu_reply_does_not_record_assistant_reply_delivered() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (_, run, session, approved) = setup_feishu_approval(&journal, "sf_f", "rf_f")?;
    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    journal.fail_outbox_dispatch(
        &approved.intent().invocation_id,
        &run.id,
        Some(&session.id),
        "failed",
    )?;
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        0
    );
    Ok(())
}

fn link_turn(
    j: &JournalStore,
    sid: &SessionId,
    ev: &str,
    ut: &str,
    rid: &str,
    rt: &str,
) -> Result<()> {
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
    let corr = format!("reply:{rid}");
    j.append_event(
        JournalEventKind::RunStarted,
        Some(&r),
        Some(sid),
        Some(ev),
        json!({"run_id":rid}),
    )?;
    j.append_event(JournalEventKind::AssistantReplyDelivered, Some(&r), Some(sid), Some(&corr), json!({"session_id":sid.0,"run_id":rid,"invocation_id":format!("reply:{rid}"),"channel":"cli","text":rt}))?;
    Ok(())
}

#[test]
fn conversation_turns_pair_by_run_id_not_delivery_order() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn(&j, &s, "ev_a", "user A", "r_a", "reply A")?;
    link_turn(&j, &s, "ev_b", "user B", "r_b", "reply B")?;
    let t = j.recent_conversation_turns(&s, 10, None)?;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0, "user A");
    assert_eq!(t[0].1, "reply A");
    assert_eq!(t[1].0, "user B");
    assert_eq!(t[1].1, "reply B");
    Ok(())
}

#[test]
fn conversation_turns_are_session_isolated() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let sa = SessionId("a".into());
    let sb = SessionId("b".into());
    link_turn(&j, &sa, "e1", "hello A", "r1", "rep A")?;
    link_turn(&j, &sb, "e2", "hello B", "r2", "rep B")?;
    assert_eq!(j.recent_conversation_turns(&sa, 10, None)?.len(), 1);
    Ok(())
}

#[test]
fn conversation_turns_exclude_failed_and_incomplete_runs() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn(&j, &s, "e1", "user 1", "r1", "reply 1")?;
    j.append_event(
        JournalEventKind::RunStarted,
        Some(&RunId("r2".into())),
        Some(&s),
        Some("e2"),
        json!({"run_id":"r2"}),
    )?;
    j.append_event(
        JournalEventKind::RunFailed,
        Some(&RunId("r3".into())),
        Some(&s),
        Some("e3"),
        json!({"status":"Failed"}),
    )?;
    assert_eq!(j.recent_conversation_turns(&s, 10, None)?.len(), 1);
    Ok(())
}

#[test]
fn conversation_turns_preserve_unicode_newlines_and_json_like_text() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn(
        &j,
        &s,
        "e1",
        "hello\nworld ✅",
        "r1",
        "{\"status\":\"ok\"} endpoint=http://127.0.0.1",
    )?;
    let t = j.recent_conversation_turns(&s, 10, None)?;
    assert_eq!(t[0].0, "hello\nworld ✅");
    assert!(t[0].1.contains("{\"status\":\"ok\"}"));
    Ok(())
}

#[test]
fn conversation_turn_limit_one_keeps_latest_complete_turn() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn(&j, &s, "e1", "u1", "r1", "r1")?;
    link_turn(&j, &s, "e2", "u2", "r2", "r2")?;
    assert_eq!(j.recent_conversation_turns(&s, 1, None)?[0].0, "u2");
    Ok(())
}

#[test]
fn incomplete_turn_does_not_displace_complete_turn() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn(&j, &s, "e1", "complete", "r1", "reply")?;
    j.append_event(
        JournalEventKind::RunStarted,
        Some(&RunId("r2".into())),
        Some(&s),
        Some("e2"),
        json!({"run_id":"r2"}),
    )?;
    assert_eq!(j.recent_conversation_turns(&s, 1, None)?[0].0, "complete");
    Ok(())
}

#[test]
fn current_run_is_excluded_from_recent_turns() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn(&j, &s, "ev_p", "prior", "r_p", "reply")?;
    link_turn(&j, &s, "ev_c", "current", "r_c", "rep")?;
    assert_eq!(
        j.recent_conversation_turns(&s, 10, Some("ev_c"))?[0].0,
        "prior"
    );
    Ok(())
}

#[test]
fn assistant_reply_delivered_rollback_removes_all() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (run, session, approved) = setup_stdout_approval(&journal)?;
    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    let receipt = Receipt {
        invocation_id: approved.intent().invocation_id.clone(),
        status: ReceiptStatus::Succeeded,
        output: json!({"status":"sent"}),
        external_ref: None,
        occurred_at: Utc::now(),
    };
    journal.succeed_outbox_dispatch(&receipt, &run.id, Some(&session.id))?;
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        1
    );
    assert!(journal
        .succeed_outbox_dispatch(&receipt, &run.id, Some(&session.id))
        .is_err());
    assert_eq!(
        journal
            .events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn ordinary_tool_receipt_does_not_record_assistant_reply_delivered() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let sid = common::test_session(&common::test_config()).id;
    let run = common::runtime_run(&RunId("rt".into()), &sid);
    j.insert_run(&run)?;
    j.append_event(
        JournalEventKind::ReceiptReceived,
        Some(&run.id),
        Some(&sid),
        Some("inv"),
        json!({"status":"Succeeded","output_kind":"text"}),
    )?;
    assert_eq!(
        j.events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        0
    );
    Ok(())
}

#[test]
fn connector_secret_fields_not_in_assistant_reply_delivered() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let (run, session, approved) = setup_stdout_approval(&journal)?;
    journal.queue_outbox_dispatch(&approved, Some(&session.id))?;
    journal.start_outbox_dispatch(&approved, Some(&session.id))?;
    journal.succeed_outbox_dispatch(
        &Receipt {
            invocation_id: approved.intent().invocation_id.clone(),
            status: ReceiptStatus::Succeeded,
            output: json!({"status":"sent","SECRET_TOKEN":"x","/path":"y","giant":"x".repeat(100)}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
        &run.id,
        Some(&session.id),
    )?;
    let ev = journal.events()?;
    let d: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
        .collect();
    assert!(d[0].payload.get("SECRET_TOKEN").is_none());
    Ok(())
}
