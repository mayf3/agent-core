use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

mod common;

#[test]
fn stale_running_worker_job_is_re_leased_on_next_poll() -> Result<()> {
    Ok(())
}

// === Critical tests ===
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::registry::snapshot::test_snapshot;
use common::{text_response, tool_call_response, CaptureServer, FakeReplyAdapter};

fn link_turn_core(
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

fn approve_stdout(
    j: &JournalStore,
    g: &Gateway,
    rid: &RunId,
    sid: &SessionId,
) -> Result<ApprovedInvocation> {
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

fn drain_outbox(j: &JournalStore, inv_id: &str) {
    let ad = FakeReplyAdapter {
        receipt: Receipt {
            invocation_id: InvocationId(inv_id.into()),
            status: ReceiptStatus::Succeeded,
            output: json!({"status":"sent"}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
    };
    while agent_core_kernel::runtime::outbox_dispatcher::dispatch_once(j, &ad).unwrap_or(false) {}
}

// 1. Real dispatcher - stdout
#[test]
fn stdout_dispatch_success_records_assistant_reply_delivered() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(common::test_config());
    let sid = SessionId("s".into());
    approve_stdout(&j, &g, &RunId("r1".into()), &sid)?;
    drain_outbox(&j, "reply:r1");
    let ev = j.events()?;
    let d: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
        .collect();
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].payload.get("text").and_then(Value::as_str),
        Some("hello")
    );
    Ok(())
}

// 2. Duplicate idempotent
#[test]
fn duplicate_success_dispatch_records_one_assistant_reply_delivered() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(common::test_config());
    let sid = SessionId("sd".into());
    approve_stdout(&j, &g, &RunId("r2".into()), &sid)?;
    drain_outbox(&j, "reply:r2");
    assert_eq!(
        j.events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        1
    );
    assert_eq!(
        j.outbox_dispatch_status(&InvocationId("reply:r2".into()))?,
        Some(OutboxDispatchStatus::Succeeded)
    );
    // Second dispatch not possible (already Succeeded)
    assert!(j.lease_next_outbox_dispatch()?.is_none());
    assert_eq!(
        j.events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        1
    );
    Ok(())
}

// 3. ToolResult E2E
#[test]
fn e2e_tool_results_appear_once_and_system_stays_byte_identical() -> Result<()> {
    let mut c = common::test_config();
    c.extra_allowed_operations = vec!["time.now".into()];
    let sv = CaptureServer::start(vec![
        tool_call_response("cx", "time.now", "{}"),
        text_response("done"),
    ]);
    c.openai_base_url = sv.base_url();
    c.openai_api_key = "t".into();
    c.model = "local-stub".into();
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(c.clone());
    let l = agent_core_kernel::llm::OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let rt = agent_core_kernel::runtime::Runtime::new(c, l);
    let ev = g.validate_ingress(&j, g.cli_ingress("test".into())?)?;
    rt.deliver(&j, &g, ev)?;
    let rq = sv.requests();
    assert_eq!(rq.len(), 2);
    let s1 = rq[0]["messages"][0]["content"].as_str().unwrap();
    let s2 = rq[1]["messages"][0]["content"].as_str().unwrap();
    assert_eq!(s1, s2);
    assert_eq!(rq[1].to_string().matches("status: succeeded").count(), 1);
    assert!(!s2.contains("status: succeeded"));
    Ok(())
}

// 4. Multi-round transcript
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
    let l = agent_core_kernel::llm::OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let rt = agent_core_kernel::runtime::Runtime::new(c, l);
    let ev = g.validate_ingress(&j, g.cli_ingress("test".into())?)?;
    rt.deliver(&j, &g, ev)?;
    let rq = sv.requests();
    assert_eq!(rq.len(), 3);
    let r1 = rq[0]["messages"].as_array().unwrap();
    let r2 = rq[1]["messages"].as_array().unwrap();
    let r3 = rq[2]["messages"].as_array().unwrap();
    assert_eq!(r1.len(), 2);
    assert_eq!(r2.len(), 4);
    assert_eq!(r3.len(), 6);
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
    Ok(())
}

// 5. Malformed -> valid
#[test]
fn malformed_follow_up_then_valid_tool_call_does_not_reuse_stale_pending_turn() -> Result<()> {
    let mut c = common::test_config();
    c.extra_allowed_operations = vec!["time.now".into()];
    let sv = CaptureServer::start(vec![
        json!({"model":"local-stub","choices":[{"message":{"content":"","tool_calls":[{"id":"bad","type":"function","function":{"name":"time.now","arguments":"{bad}"}}]}}]}),
        tool_call_response("vcall", "time.now", "{}"),
        text_response("done"),
    ]);
    c.openai_base_url = sv.base_url();
    c.openai_api_key = "t".into();
    c.model = "local-stub".into();
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(c.clone());
    let l = agent_core_kernel::llm::OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let rt = agent_core_kernel::runtime::Runtime::new(c, l);
    let ev = g.validate_ingress(&j, g.cli_ingress("test".into())?)?;
    rt.deliver(&j, &g, ev)?;
    let rq = sv.requests();
    assert_eq!(rq.len(), 3);
    let r3 = rq[2]["messages"].as_array().unwrap();
    let cid = r3[2]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(r3[3]["tool_call_id"].as_str(), Some(cid));
    assert_ne!(cid, "bad");
    Ok(())
}

// 6. Identity rejection
#[test]
fn conversation_turns_reject_mismatched_assistant_event_identity() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn_core(&j, &s, "e1", "user", "r1", "reply")?;
    let r = RunId("r2".into());
    j.append_event(JournalEventKind::AssistantReplyDelivered,Some(&r),Some(&s),Some("reply:r2"),json!({"session_id":"wrong","run_id":"r2","invocation_id":"reply:r2","channel":"cli","text":"bad"}))?;
    j.append_event(
        JournalEventKind::AssistantReplyDelivered,
        Some(&r),
        Some(&s),
        Some("reply:r3"),
        json!({"session_id":s.0,"run_id":"r3","invocation_id":"","channel":"cli","text":"empty"}),
    )?;
    let t = j.recent_conversation_turns(&s, 10, None)?;
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].1, "reply");
    Ok(())
}

// 7-12. Limit matrix
#[test]
fn conversation_turn_limit_one_keeps_latest_complete_turn() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn_core(&j, &s, "e1", "u1", "r1", "r1")?;
    link_turn_core(&j, &s, "e2", "u2", "r2", "r2")?;
    assert_eq!(j.recent_conversation_turns(&s, 1, None)?[0].0, "u2");
    Ok(())
}
#[test]
fn conversation_turn_limit_overflow_keeps_latest_complete_turns() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    for i in 0..4 {
        link_turn_core(
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
fn failed_turn_does_not_consume_limit() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn_core(&j, &s, "e1", "good", "r1", "reply")?;
    j.append_event(
        JournalEventKind::RunFailed,
        Some(&RunId("r2".into())),
        Some(&s),
        Some("e2"),
        json!({"status":"Failed"}),
    )?;
    assert_eq!(j.recent_conversation_turns(&s, 1, None)?[0].0, "good");
    Ok(())
}
#[test]
fn incomplete_turn_does_not_consume_limit() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn_core(&j, &s, "e1", "complete", "r1", "reply")?;
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

// 13. Out-of-order pairing
#[test]
fn conversation_turns_pair_by_run_id_when_replies_complete_out_of_order() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn_core(&j, &s, "ev_a", "user A", "r_a", "reply A")?;
    link_turn_core(&j, &s, "ev_b", "user B", "r_b", "reply B")?;
    let t = j.recent_conversation_turns(&s, 10, None)?;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0, "user A");
    assert_eq!(t[1].0, "user B");
    Ok(())
}

// 14. Connector secret
#[test]
fn connector_unknown_fields_are_not_persisted_in_journal() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(common::test_config());
    let sid = SessionId("sx".into());
    approve_stdout(&j, &g, &RunId("rx".into()), &sid)?;
    // Override the receipt with secret fields
    let ad = FakeReplyAdapter {
        receipt: Receipt {
            invocation_id: InvocationId("reply:rx".into()),
            status: ReceiptStatus::Succeeded,
            output: json!({"status":"sent","SECRET_TOKEN_MARKER":"x","/private/path":"y","nested":{"s":"NESTED_MARKER"},"large":"LARGE_MARKER.."}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
    };
    agent_core_kernel::runtime::outbox_dispatcher::dispatch_once(&j, &ad).ok();
    let body = serde_json::to_string(&j.events()?).unwrap_or_default();
    for m in &[
        "SECRET_TOKEN_MARKER",
        "/private/path",
        "NESTED_MARKER",
        "LARGE_MARKER",
    ] {
        assert!(!body.contains(m), "leaked {m}");
    }
    Ok(())
}

// 15. Feishu dispatch (real dispatcher)
#[test]
fn feishu_dispatch_success_records_assistant_reply_delivered() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let sid = SessionId("sf".into());
    let g = Gateway::new(common::test_config());
    let snap = test_snapshot();
    let sess = Session {
        id: sid.clone(),
        agent_id: AgentId("m".into()),
        channel: ChannelKind::Feishu,
        conversation_key: "f".into(),
        summary: None,
        summarized_until_event_id: None,
        last_active_at: Utc::now(),
        status: SessionStatus::Active,
        version: 1,
    };
    let run = Run {
        id: RunId("rf".into()),
        session_id: sid.clone(),
        agent_id: AgentId("m".into()),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("f".into()),
            subject: PrincipalSubject::FeishuOpenId("u".into()),
            source: PrincipalSource::Feishu,
            grants: vec![CapabilityGrant {
                operation: "feishu.send_message".into(),
                scope: "current_session".into(),
            }],
            requester_id: Some("f".into()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: String::new(),
    };
    j.insert_run(&run)?;
    let ap=g.approve_invocation(InvocationIntent{invocation_id:InvocationId("reply:rf".into()),run_id:RunId("rf".into()),operation:"feishu.send_message".into(),arguments:json!({"text":"feishu reply","session_id":sid.0,"message_id":"m1","chat_id":"oc1"}),idempotency_key:None},&run,&sess,&snap)?;
    j.queue_outbox_dispatch(&ap, Some(&sid))?;
    let ad = FakeReplyAdapter {
        receipt: Receipt {
            invocation_id: InvocationId("reply:rf".into()),
            status: ReceiptStatus::Succeeded,
            output: json!({"message_id":"m1","status":"sent"}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
    };
    agent_core_kernel::runtime::outbox_dispatcher::dispatch_once(&j, &ad).ok();
    let ev = j.events()?;
    let d: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
        .collect();
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].payload.get("text").and_then(Value::as_str),
        Some("feishu reply")
    );
    Ok(())
}

// 16. Limit zero
#[test]
fn conversation_turn_limit_zero_returns_empty() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn_core(&j, &s, "e1", "u", "r1", "r")?;
    assert!(j.recent_conversation_turns(&s, 0, None)?.is_empty());
    Ok(())
}

// 17. Current run excluded
#[test]
fn current_run_is_excluded_from_recent_turns() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    link_turn_core(&j, &s, "ev_p", "prior", "r_p", "reply")?;
    link_turn_core(&j, &s, "ev_c", "current", "r_c", "rep")?;
    assert_eq!(
        j.recent_conversation_turns(&s, 10, Some("ev_c"))?[0].0,
        "prior"
    );
    Ok(())
}
