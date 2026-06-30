//! External harness pinning / barrier / failure-path tests.
//! All tests use real Runtime::deliver. Reuses helpers from sibling module.

use super::external_harness_runtime::{
    captured_follow_ups, captured_system, config, harness_200, register_and_enable,
    start_responder, CaptureToolsLlm,
};
use crate::harness::control::{HarnessChangeAction, HarnessChangeIntent};
use crate::harness::manifest::HarnessManifest;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

// ═══ Enable: Run A/S1 → enable → Run B/S2 (Condvar barrier) ═══

struct BlockerLlm {
    cap: Arc<Mutex<Vec<serde_json::Value>>>,
    ready: Arc<(Mutex<bool>, Condvar)>,
    released: Arc<AtomicBool>,
    tool: bool,
}
impl crate::llm::LlmClient for BlockerLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.cap.lock().unwrap().push(json!({"system":captured_system(&input),"provider_tools":input.provider_tools,"follow_ups":captured_follow_ups(&input),"follow_up_count":input.follow_ups.len()}));
        *self.ready.0.lock().unwrap() = true;
        self.ready.1.notify_one();
        let mut g2 = self.ready.0.lock().unwrap();
        while !self.released.load(Ordering::SeqCst) {
            g2 = self.ready.1.wait(g2).unwrap();
        }
        if self.tool {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                    id: "c".into(),
                    operation: "external.time_now".into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(crate::llm::ProviderToolTurn {
                    endpoint: crate::llm::EndpointChoice::Primary,
                    provider_tool_call_id: "cr".into(),
                    wire_name: "external.time_now".into(),
                    canonical_operation: "external.time_now".into(),
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "a_done".into(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

#[test]
fn external_harness_enable_pins_only_future_runs() -> Result<()> {
    let hb = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1}});
    let (ep, _) = start_responder(&harness_200(&hb.to_string()))?;
    let j = Arc::new(crate::journal::JournalStore::in_memory()?);
    let g = Arc::new(crate::gateway::Gateway::new(config()));
    let s1 = j.current_registry_snapshot_id()?;
    let cap_a = Arc::new(Mutex::new(Vec::new()));
    let rdy_a: Arc<(Mutex<bool>, Condvar)> = Arc::new((Mutex::new(false), Condvar::new()));
    let rel_a = Arc::new(AtomicBool::new(false));
    let j_a = j.clone();
    let g_a = g.clone();
    let rdy_a2 = rdy_a.clone();
    let rel_a2 = rel_a.clone();
    let cap_a2 = cap_a.clone();
    let h_a = thread::spawn(move || -> Result<_> {
        super::Runtime::new(
            config(),
            BlockerLlm {
                cap: cap_a2,
                ready: rdy_a2,
                released: rel_a2,
                tool: false,
            },
        )
        .deliver(
            &*j_a,
            &*g_a,
            g_a.validate_ingress(&*j_a, g_a.cli_ingress("t?".into())?)
                .unwrap(),
        )
    });
    // Wait for Run A to block.
    let mut rg = rdy_a.0.lock().unwrap();
    while !*rg {
        rg = rdy_a.1.wait(rg).unwrap();
    }
    drop(rg);
    // Verify Run A pinned to S1.
    let run_a = {
        let ev = j.events()?;
        let r = ev
            .iter()
            .find(|e| e.kind == crate::domain::JournalEventKind::RunStarted)
            .unwrap();
        j.run(&crate::domain::RunId(
            r.payload["run_id"].as_str().unwrap_or("").to_string(),
        ))?
        .unwrap()
    };
    assert_eq!(run_a.registry_snapshot_id, s1);
    assert!(!run_a
        .principal
        .grants
        .iter()
        .any(|g| g.operation == "external.time_now"));
    // Enable → S2 while Run A is blocked.
    register_and_enable(&*j, &*g, &ep)?;
    let s2 = j.current_registry_snapshot_id()?;
    assert_ne!(s1, s2);
    rel_a.store(true, Ordering::SeqCst);
    rdy_a.1.notify_one();
    let oa = h_a.join().unwrap()?;
    assert_eq!(oa.output, "a_done");
    // Verify Run A provider tools lack external.time_now.
    for (i, c) in cap_a.lock().unwrap().iter().enumerate() {
        let names: Vec<&str> = c["provider_tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !names.contains(&"external.time_now"),
            "Run A round {i} has external.time_now"
        );
    }
    // Run B (same Session) → S2, has external.time_now.
    let cap_b = Arc::new(Mutex::new(Vec::new()));
    let ob = super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: cap_b.clone(),
            first: AtomicBool::new(true),
        },
    )
    .deliver(
        &*j,
        &*g,
        g.validate_ingress(&*j, g.cli_ingress("t?".into())?)?,
    )?;
    assert!(!ob.output.trim().is_empty());
    let run_b = j.run(&ob.run_id)?.unwrap();
    assert_eq!(
        run_b.registry_snapshot_id,
        j.current_registry_snapshot_id()?
    );
    assert!(run_b
        .principal
        .grants
        .iter()
        .any(|g| g.operation == "external.time_now"));
    assert!(j
        .load_registry_snapshot(&s1)?
        .lookup("external.time_now")
        .is_none());
    Ok(())
}

// ═══ Disable: Run B/S2 → disable → Run C/S3 (harness barrier) ═══

#[test]
fn external_harness_disable_pins_only_future_runs() -> Result<()> {
    let j = Arc::new(crate::journal::JournalStore::in_memory()?);
    let g = Arc::new(crate::gateway::Gateway::new(config()));
    let s1 = j.current_registry_snapshot_id()?;
    // Barrier responder: accept, read, notify, block until released, then respond.
    let bp: Arc<(Mutex<bool>, Condvar)> = Arc::new((Mutex::new(false), Condvar::new()));
    let bq = Arc::new(AtomicBool::new(false));
    let l2 = TcpListener::bind("127.0.0.1:0")?;
    let hp = l2.local_addr()?.port();
    {
        let bp2 = bp.clone();
        let bq2 = bq.clone();
        thread::spawn(move || {
            if let Ok((mut s, _)) = l2.accept() {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b);
                *bp2.0.lock().unwrap() = true;
                bp2.1.notify_one();
                let mut g2 = bp2.0.lock().unwrap();
                while !bq2.load(Ordering::SeqCst) {
                    g2 = bp2.1.wait(g2).unwrap();
                }
                let body = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1}});
                let _=s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.to_string().len(),body.to_string()).as_bytes());
            }
        });
    }
    // Register + enable on barrier endpoint → S2.
    let ep = format!("http://127.0.0.1:{hp}/execute");
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "t2".into(),
        artifact_digest: format!("sha256:{}", "c".repeat(64)),
        protocol_version: "external-harness-v1".into(),
        endpoint: ep,
        operation_name: "external.time_now".into(),
        description: "t".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"iso":{"type":"string"},"epoch_ms":{"type":"integer"}},"required":["iso","epoch_ms"],"additionalProperties":false}),
        idempotent: true,
        created_at: Utc::now(),
    };
    let mid = m.compute_manifest_id()?;
    m.manifest_id = mid.clone();
    j.register_harness_manifest(&m)?;
    j.enable_harness(&g.approve_harness_change(HarnessChangeIntent {
        action: HarnessChangeAction::Enable,
        manifest_id: mid.clone(),
        expected_snapshot_id: j.current_registry_snapshot_id()?,
        requested_by: "ipc_operator".into(),
    })?)?;
    let s2 = j.current_registry_snapshot_id()?;
    assert_ne!(s1, s2);
    // Run B pinned to S2, calls external.time_now → harness blocks.
    let cap_b = Arc::new(Mutex::new(Vec::new()));
    let cap_b_early = cap_b.clone();
    let j_b = j.clone();
    let g_b = g.clone();
    let h_b = thread::spawn(move || -> Result<_> {
        super::Runtime::new(
            config(),
            CaptureToolsLlm {
                captured: cap_b_early.clone(),
                first: AtomicBool::new(true),
            },
        )
        .deliver(
            &*j_b,
            &*g_b,
            g_b.validate_ingress(&*j_b, g_b.cli_ingress("t?".into())?)
                .unwrap(),
        )
    });
    // Wait for harness responder to accept.
    let mut rg = bp.0.lock().unwrap();
    while !*rg {
        rg = bp.1.wait(rg).unwrap();
    }
    drop(rg);
    // ── Pre-disable snapshot: lock Run B's registry_snapshot_id ──
    let run_b = {
        let ev = j.events()?;
        let r = ev
            .iter()
            .find(|e| e.kind == crate::domain::JournalEventKind::RunStarted)
            .expect("Run B RunStarted");
        j.run(&crate::domain::RunId(
            r.payload["run_id"].as_str().unwrap_or("").to_string(),
        ))?
        .unwrap()
    };
    let run_b_id = run_b.id.clone();
    assert!(
        run_b
            .principal
            .grants
            .iter()
            .any(|g| g.operation == "external.time_now"),
        "pre-disable: Run B must have external.time_now grant"
    );
    assert_eq!(
        run_b.registry_snapshot_id, s2,
        "pre-disable: Run B pinned to S2"
    );
    // Run B's provider_tools captured in first LLM round must include external.time_now.
    let caps = cap_b.lock().unwrap();
    assert!(
        caps.iter().any(|c| c["provider_tools"]
            .as_array()
            .map(|a| a
                .iter()
                .any(|t| t["function"]["name"] == "external.time_now"))
            .unwrap_or(false)),
        "pre-disable: Run B provider_tools must contain external.time_now"
    );
    drop(caps);
    // ── Disable harness → S3 while Run B is running ──
    let cur = j.current_registry_snapshot_id()?;
    let rd = j.disable_harness(&g.approve_harness_change(HarnessChangeIntent {
        action: HarnessChangeAction::Disable,
        manifest_id: mid.clone(),
        expected_snapshot_id: cur,
        requested_by: "ipc_operator".into(),
    })?)?;
    let s3 = rd.active_snapshot_id;
    assert!(rd.changed);
    assert_ne!(s2, s3, "S2 != S3");
    assert_eq!(
        j.current_registry_snapshot_id()?,
        s3,
        "active snapshot == S3 after disable"
    );
    // ── Post-disable, pre-release: Run B must still have S2 ──
    let run_b_after = j.run(&run_b_id)?.unwrap();
    assert_eq!(
        run_b_after.registry_snapshot_id, s2,
        "post-disable: Run B still pinned to S2"
    );
    assert!(
        run_b_after
            .principal
            .grants
            .iter()
            .any(|g| g.operation == "external.time_now"),
        "post-disable: Run B grants unchanged"
    );
    // ── Release the responder (Run B completes) ──
    bq.store(true, Ordering::SeqCst);
    bp.1.notify_one();
    let ob = h_b.join().unwrap()?;
    assert!(!ob.output.trim().is_empty());
    // Run B completed with S2 (still has the harness op): Receipt Succeeded.
    let ev_b = j.events()?;
    let r_b: Vec<_> = ev_b
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(
        r_b.iter()
            .filter(|e| e.payload["status"] == "Succeeded")
            .count(),
        1,
        "Run B must have one Succeeded Receipt"
    );
    // Run C (same Session) → S3, no external.time_now.
    let cap_c = Arc::new(Mutex::new(Vec::new()));
    let oc = super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: cap_c.clone(),
            first: AtomicBool::new(true),
        },
    )
    .deliver(
        &*j,
        &*g,
        g.validate_ingress(&*j, g.cli_ingress("t?".into())?)?,
    )?;
    let run_c = j.run(&oc.run_id)?.unwrap();
    assert_eq!(run_c.registry_snapshot_id, s3);
    assert!(!run_c
        .principal
        .grants
        .iter()
        .any(|g| g.operation == "external.time_now"));
    // Direct assertion: every captured provider request from Run C
    // must lack external.time_now (not just the principal/snapshot).
    let caps_c = cap_c.lock().unwrap();
    assert!(
        !caps_c.is_empty(),
        "Run C must make at least one provider request"
    );
    for (round, input) in caps_c.iter().enumerate() {
        let has_external_time = input["provider_tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .any(|tool| tool["function"]["name"].as_str() == Some("external.time_now"))
            })
            .unwrap_or(false);
        assert!(
            !has_external_time,
            "Run C round {round} must not expose external.time_now"
        );
    }
    assert_ne!(s1, s2, "S1 != S2");
    assert_ne!(s2, s3, "S2 != S3");
    assert!(j
        .load_registry_snapshot(&s1)?
        .lookup("external.time_now")
        .is_none());
    assert!(j
        .load_registry_snapshot(&s2)?
        .lookup("external.time_now")
        .is_some());
    Ok(())
}
