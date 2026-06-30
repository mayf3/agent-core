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

fn fast_cfg() -> crate::config::KernelConfig {
    let mut c = config();
    c.harness_read_timeout_ms = 1000;
    c
}

const SM: &[&str] = &[
    "SECRET_TOKEN_MARKER",
    "/private/internal/path",
    "fake_receipt",
    "fake_journal_event",
    "fake_status",
    "fake_decision_id",
    "fake_occurred_at",
];

// ═══ Malformed JSON through Runtime ═══

#[test]
fn external_harness_malformed_json_through_runtime_records_failed() -> Result<()> {
    let (ep, _) = start_responder(&harness_200("{not valid json"))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    let cap = Arc::new(Mutex::new(Vec::new()));
    super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: cap.clone(),
            first: AtomicBool::new(true),
        },
    )
    .deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(
        r[0].payload["output"]["error_category"],
        "malformed_response"
    );
    assert_eq!(
        ev.iter()
            .filter(
                |e| e.kind == crate::domain::JournalEventKind::ReceiptReceived
                    && e.payload["status"] == "Succeeded"
            )
            .count(),
        0
    );
    assert!(cap.lock().unwrap().iter().any(|c| c["follow_ups"]
        .as_array()
        .map(|a| a.iter().any(|fu| fu["result_content"]
            .as_str()
            .map(|s| s.contains("execution_failed"))
            .unwrap_or(false)))
        .unwrap_or(false)));
    let jt = serde_json::to_string(&ev).unwrap_or_default();
    assert!(!jt.contains("{not valid json}"));
    assert!(!jt.contains("{not valid"));
    Ok(())
}

// ═══ Fast timeout (short harness_read_timeout_ms) ═══

#[test]
fn harness_fast_timeout_through_runtime_records_failed() -> Result<()> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    let p = l.local_addr()?.port();
    let ep = format!("http://127.0.0.1:{p}/execute");
    thread::spawn(move || {
        if let Ok((s, _)) = l.accept() {
            let _h = s;
            thread::sleep(Duration::from_secs(10));
        }
    });
    thread::sleep(Duration::from_millis(50));
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(fast_cfg());
    register_and_enable(&j, &g, &ep)?;
    let cap = Arc::new(Mutex::new(Vec::new()));
    super::Runtime::new(
        fast_cfg(),
        CaptureToolsLlm {
            captured: cap.clone(),
            first: AtomicBool::new(true),
        },
    )
    .deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let r: Vec<_> = j
        .events()?
        .into_iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(r[0].payload["output"]["error_category"], "timeout");
    Ok(())
}

// ═══ 5-layer secret/unknown-field scan ═══

#[test]
fn harness_secret_fields_five_layer_scan() -> Result<()> {
    let body = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1},
        "SECRET_TOKEN_MARKER":"x","/private/internal/path":"x","fake_receipt":{"s":"ok"},
        "fake_journal_event":{"kind":"RC"},"fake_status":"ok","fake_decision_id":"d","fake_occurred_at":"2026-06-30T00:00:00Z"});
    let (ep, _) = start_responder(&harness_200(&body.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    let cap = Arc::new(Mutex::new(Vec::new()));
    let o = super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: cap.clone(),
            first: AtomicBool::new(true),
        },
    )
    .deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("t?".into())?)?)?;
    let ev = j.events()?;
    let jt = serde_json::to_string(&ev).unwrap_or_default();
    for &m in SM {
        assert!(!jt.contains(m), "journal leaked {m}");
    }
    let caps = cap.lock().unwrap();
    for c in caps.iter() {
        if let Some(fus) = c["follow_ups"].as_array() {
            for fu in fus {
                if let Some(rc) = fu["result_content"].as_str() {
                    for &m in SM {
                        assert!(!rc.contains(m), "ToolResult leaked {m}");
                    }
                }
            }
        }
    }
    for c in caps.iter() {
        let s = serde_json::to_string(c).unwrap_or_default();
        for &m in SM {
            assert!(!s.contains(m), "LlmInput leaked {m}");
        }
    }
    assert!(!o.output.contains("SECRET_TOKEN_MARKER"));
    Ok(())
}

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
    let j_b = j.clone();
    let g_b = g.clone();
    let h_b = thread::spawn(move || -> Result<_> {
        super::Runtime::new(
            config(),
            CaptureToolsLlm {
                captured: cap_b.clone(),
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
    // Disable harness → S3 while Run B is running.
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
    // S3 may equal S1 if both are the harness-less baseline (deterministic hash).
    bq.store(true, Ordering::SeqCst);
    bp.1.notify_one();
    let ob = h_b.join().unwrap()?;
    assert!(!ob.output.trim().is_empty());
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
