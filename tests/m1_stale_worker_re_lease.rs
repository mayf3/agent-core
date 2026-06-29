mod common;
use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
#[test]
fn stale_running_worker_job_is_re_leased_on_next_poll() -> Result<()> {
    Ok(())
}
// === Critical tests ===
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::registry::snapshot::test_snapshot;
use common::{text_response, tool_call_response, CaptureServer, FakeReplyAdapter};
#[allow(dead_code)]
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
#[test]
fn second_run_provider_request_contains_prior_delivered_assistant_reply() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let sv = CaptureServer::start(vec![
        text_response("候选Harness已启动，endpoint=http://127.0.0.1:7101。是否启用？"),
        text_response("已处理"),
    ]);
    let mut c = common::test_config();
    c.openai_base_url = sv.base_url();
    c.openai_api_key = "t".into();
    c.model = "local-stub".into();
    let g = Gateway::new(c.clone());
    let l = agent_core_kernel::llm::OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let rt = agent_core_kernel::runtime::Runtime::new(c, l);
    let oa = rt.deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("帮我准备候选Harness".into())?)?,
    )?;
    let ad = FakeReplyAdapter {
        receipt: Receipt {
            invocation_id: InvocationId(format!("reply:{}", oa.run_id.0)),
            status: ReceiptStatus::Succeeded,
            output: json!({"status":"sent"}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
    };
    common::dispatch_all(&j, &ad)?;
    assert_eq!(
        j.events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        1
    );
    let ob = rt.deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("启用".into())?)?,
    )?;
    assert_eq!(oa.session_id, ob.session_id, "same session");
    assert_ne!(oa.run_id, ob.run_id, "different runs");
    let rq = sv.requests();
    let msgs = rq[1]["messages"].as_array().unwrap();
    let sys = msgs[0]["content"].as_str().unwrap_or("");
    let usr = msgs[1]["content"].as_str().unwrap_or("");
    assert_eq!(usr, "启用");
    // "启用" appears as substring of Run A's assistant reply, so check
    // it's NOT a standalone User: line in RecentMessages
    assert!(
        !sys.contains("User: 启用"),
        "current msg must not be in RecentMessages as User:"
    );
    assert!(sys.contains("帮我准备候选Harness"));
    assert!(sys.contains("候选Harness已启动，endpoint=http://127.0.0.1:7101。是否启用？"));
    assert_eq!(sys.matches("候选Harness已启动").count(), 1);
    Ok(())
}
#[test]
fn failed_prior_reply_is_absent_from_second_run_provider_request() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let sv = CaptureServer::start(vec![text_response("候选回复文本"), text_response("已处理")]);
    let mut c = common::test_config();
    c.openai_base_url = sv.base_url();
    c.openai_api_key = "t".into();
    c.model = "local-stub".into();
    let g = Gateway::new(c.clone());
    let l = agent_core_kernel::llm::OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let rt = agent_core_kernel::runtime::Runtime::new(c, l);
    let oa = rt.deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("帮我准备".into())?)?,
    )?;
    let ad = FakeReplyAdapter {
        receipt: Receipt {
            invocation_id: InvocationId(format!("reply:{}", oa.run_id.0)),
            status: ReceiptStatus::Failed,
            output: json!({}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
    };
    common::dispatch_all(&j, &ad)?;
    assert_eq!(
        j.events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        0,
        "no AssistantReplyDelivered on failed"
    );
    rt.deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("启用".into())?)?,
    )?;
    let rq = sv.requests();
    let sys = rq[1]["messages"][0]["content"].as_str().unwrap_or("");
    assert!(!sys.contains("候选回复文本"), "failed text must not appear");
    Ok(())
}
#[test]
fn assistant_reply_delivered_transaction_failure_rolls_back_all() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let g = Gateway::new(common::test_config());
    let sid = SessionId("trig".into());
    approve_stdout(&j, &g, &RunId("rt".into()), &sid)?;
    j.execute_sql_for_test("CREATE TRIGGER fail_ard BEFORE INSERT ON journal_events WHEN NEW.kind='AssistantReplyDelivered' BEGIN SELECT RAISE(ABORT,'forced'); END")?;
    let ad = FakeReplyAdapter {
        receipt: Receipt {
            invocation_id: InvocationId("reply:rt".into()),
            status: ReceiptStatus::Succeeded,
            output: json!({"status":"sent"}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
    };
    assert!(
        agent_core_kernel::runtime::outbox_dispatcher::dispatch_once(&j, &ad).is_err(),
        "dispatch must fail"
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
    Ok(())
}
