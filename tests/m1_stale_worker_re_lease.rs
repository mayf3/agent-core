use agent_core_kernel::domain::*;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

mod common;

#[test]
fn stale_running_worker_job_is_re_leased_on_next_poll() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let event_id = EventId("evt_re_lease".to_string());
    let job_id = journal.enqueue_worker_job(&event_id)?;
    let first = journal.lease_next_worker_job()?;
    assert_eq!(first.as_ref().map(|e| &e.0), Some(&event_id.0));
    let second = journal.lease_next_worker_job()?;
    assert!(second.is_none(), "a live lease must not be re-acquired");
    journal.expire_worker_lease_for_test(&job_id)?;
    let re_leased = journal.lease_next_worker_job()?;
    assert_eq!(
        re_leased.as_ref().map(|e| &e.0),
        Some(&event_id.0),
        "stale job must be re-leased"
    );
    assert_eq!(
        journal.worker_job_stale_count()?,
        0,
        "after re-leasing, job is no longer stale"
    );
    Ok(())
}

use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::llm::OpenAiCompatibleLlm;
use agent_core_kernel::registry::snapshot::test_snapshot;
use agent_core_kernel::runtime::outbox_dispatcher::dispatch_once;
use agent_core_kernel::runtime::Runtime;
use common::{text_response, CaptureServer, FakeReplyAdapter};

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
    let t = j.recent_conversation_turns(&s, 10, None)?;
    assert_eq!(t.len(), 1);
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
fn conversation_turn_limit_zero_returns_empty() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "u", "r1", "r")?;
    assert!(j.recent_conversation_turns(&s, 0, None)?.is_empty());
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
fn conversation_turn_limit_two_preserves_order() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let s = common::test_session(&common::test_config()).id;
    lt(&j, &s, "e1", "u1", "r1", "r1")?;
    lt(&j, &s, "e2", "u2", "r2", "r2")?;
    lt(&j, &s, "e3", "u3", "r3", "r3")?;
    let t = j.recent_conversation_turns(&s, 2, None)?;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0, "u2");
    assert_eq!(t[1].0, "u3");
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
    lt(&j, &s, "e1", "good", "r1", "reply")?;
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
    apv(&j, &g, &RunId("rx".into()), &sid)?;
    let ad = FakeReplyAdapter {
        receipt: Receipt {
            invocation_id: InvocationId("reply:rx".into()),
            status: ReceiptStatus::Succeeded,
            output: json!({"status":"sent","SECRET_TOKEN_MARKER":"x","/private/internal/path":"y","nested":{"secret":"NESTED_SECRET_MARKER"},"large_unknown":"LARGE_UNKNOWN_MARKER.."}),
            external_ref: None,
            occurred_at: Utc::now(),
        },
    };
    dispatch_once(&j, &ad).ok();
    let body = serde_json::to_string(&j.events()?).unwrap_or_default();
    for m in &[
        "SECRET_TOKEN_MARKER",
        "/private/internal/path",
        "NESTED_SECRET_MARKER",
        "LARGE_UNKNOWN_MARKER",
    ] {
        assert!(!body.contains(m), "leaked {m}");
    }
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
    let l1 = OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let oa = Runtime::new(c.clone(), l1).deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("帮我准备候选Harness".into())?)?,
    )?;
    common::dispatch_all(
        &j,
        &FakeReplyAdapter {
            receipt: rcp(&format!("reply:{}", oa.run_id.0)),
        },
    )?;
    assert_eq!(
        j.events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        1
    );
    let l2 = OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let ob = Runtime::new(c, l2).deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("启用".into())?)?,
    )?;
    assert_eq!(oa.session_id, ob.session_id);
    assert_ne!(oa.run_id, ob.run_id);
    let rq = sv.requests();
    let msgs = rq[1]["messages"].as_array().unwrap();
    let sys = msgs[0]["content"].as_str().unwrap_or("");
    let usr = msgs[1]["content"].as_str().unwrap_or("");
    assert_eq!(usr, "启用");
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
    let l1 = OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    let oa = Runtime::new(c.clone(), l1).deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("帮我准备".into())?)?,
    )?;
    common::dispatch_all(
        &j,
        &FakeReplyAdapter {
            receipt: Receipt {
                invocation_id: InvocationId(format!("reply:{}", oa.run_id.0)),
                status: ReceiptStatus::Failed,
                output: json!({}),
                external_ref: None,
                occurred_at: Utc::now(),
            },
        },
    )?;
    assert_eq!(
        j.events()?
            .iter()
            .filter(|e| e.kind == JournalEventKind::AssistantReplyDelivered)
            .count(),
        0
    );
    let l2 = OpenAiCompatibleLlm::new(
        c.openai_base_url.clone(),
        c.openai_api_key.clone(),
        c.model.clone(),
        3000,
    );
    Runtime::new(c, l2).deliver(
        &j,
        &g,
        g.validate_ingress(&j, g.cli_ingress("启用".into())?)?,
    )?;
    let rq = sv.requests();
    let sys = rq[1]["messages"][0]["content"].as_str().unwrap_or("");
    assert!(!sys.contains("候选回复文本"));
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
    Ok(())
}
