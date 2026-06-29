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

// ===== Provider continuity and tool transcript E2E tests =====

// ===== E2E tests for continuity, dispatch, rollback, idempotency =====
use agent_core_kernel::llm::OpenAiCompatibleLlm;
use agent_core_kernel::runtime::Runtime;
use common::{text_response, tool_call_response, CaptureServer};

#[test]
fn e2e_multi_round_tool_loop_preserves_complete_http_transcript() -> Result<()> {
    let mut c = common::test_config();
    c.extra_allowed_operations = vec!["time.now".into()];
    let sv = CaptureServer::start(vec![
        tool_call_response("cA", "time.now", "{}"),
        tool_call_response("cB", "time.now", "{}"),
        text_response("done"),
    ]);
    c.openai_base_url = sv.base_url();
    c.openai_api_key = "t".into();
    c.model = "local-stub".into();
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(c.clone());
    let l = OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let rt = Runtime::new(c, l);
    let ev = g.validate_ingress(&j, g.cli_ingress("test".into())?)?;
    rt.deliver(&j, &g, ev)?;
    let rq = sv.requests();
    assert_eq!(rq.len(), 3, "3 provider requests");
    let r1 = rq[0]["messages"].as_array().unwrap();
    let r2 = rq[1]["messages"].as_array().unwrap();
    let r3 = rq[2]["messages"].as_array().unwrap();
    assert_eq!(r1.len(), 2);
    assert_eq!(r2.len(), 4);
    assert_eq!(r3.len(), 6);
    assert_eq!(r2[2]["role"], "assistant");
    assert_eq!(r2[3]["role"], "tool");
    assert_eq!(r3[4]["role"], "assistant");
    assert_eq!(r3[5]["role"], "tool");
    let aid = r3[2]["tool_calls"][0]["id"].as_str().unwrap();
    let bid = r3[4]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(r3[3]["tool_call_id"].as_str(), Some(aid));
    assert_eq!(r3[5]["tool_call_id"].as_str(), Some(bid));
    let s1 = r1[0]["content"].as_str().unwrap();
    let s2 = r2[0]["content"].as_str().unwrap();
    let s3 = r3[0]["content"].as_str().unwrap();
    assert_eq!(s1, s2, "R1==R2");
    assert_eq!(s2, s3, "R2==R3");
    assert_eq!(r1.iter().filter(|m| m["role"] == "tool").count(), 0);
    assert_eq!(r2.iter().filter(|m| m["role"] == "tool").count(), 1);
    assert_eq!(r3.iter().filter(|m| m["role"] == "tool").count(), 2);
    assert!(!s1.contains("status: succeeded"));
    Ok(())
}

#[test]
fn conversation_turn_limit_two_preserves_order() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn(&j, &s, "e1", "u1", "r1", "r1")?;
    link_turn(&j, &s, "e2", "u2", "r2", "r2")?;
    link_turn(&j, &s, "e3", "u3", "r3", "r3")?;
    let t = j.recent_conversation_turns(&s, 2, None)?;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0, "u2");
    assert_eq!(t[1].0, "u3");
    Ok(())
}

#[test]
fn conversation_turn_limit_overflow_keeps_latest() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    for i in 0..5 {
        link_turn(
            &j,
            &s,
            &format!("e{i}"),
            &format!("u{i}"),
            &format!("r{i}"),
            "r",
        )?;
    }
    let t = j.recent_conversation_turns(&s, 3, None)?;
    assert_eq!(t.len(), 3);
    assert_eq!(t[0].0, "u2");
    assert_eq!(t[2].0, "u4");
    Ok(())
}

#[test]
fn malformed_follow_up_then_valid_tool_call_does_not_reuse_stale_pending_turn() -> Result<()> {
    let mut c = common::test_config();
    c.extra_allowed_operations = vec!["time.now".into()];
    // Malformed tool call is rejected by gateway (no InvocationProposed).
    // Next round: valid tool call -> tool result.
    let sv = CaptureServer::start(vec![
        json!({"model":"local-stub","choices":[{"message":{"content":"","tool_calls":[{"id":"bad","type":"function","function":{"name":"time.now","arguments":"{bad}"}}]}}]}),
        tool_call_response("valid_call", "time.now", "{}"),
        text_response("done"),
    ]);
    c.openai_base_url = sv.base_url();
    c.openai_api_key = "t".into();
    c.model = "local-stub".into();
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(c.clone());
    let l = OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let rt = Runtime::new(c, l);
    let ev = g.validate_ingress(&j, g.cli_ingress("test".into())?)?;
    rt.deliver(&j, &g, ev)?;
    let rq = sv.requests();
    assert_eq!(rq.len(), 3, "3 requests");
    // Round 3 should have both assistant+tool messages (from valid call)
    // The malformed call is handled internally and doesn't add to transcript
    let r3 = rq[2]["messages"].as_array().unwrap();
    assert_eq!(
        r3.len(),
        4,
        "at least 4 messages: system+user+assistant+tool"
    );
    assert_eq!(r3[2]["role"], "assistant");
    assert_eq!(r3[3]["role"], "tool");
    // Valid call's tool_call_id matches its result
    let call_id = r3[2]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(r3[3]["tool_call_id"].as_str(), Some(call_id));
    // The result is NOT using the malformed call's pending turn
    assert_ne!(call_id, "bad", "must not reuse malformed call_id");
    Ok(())
}
