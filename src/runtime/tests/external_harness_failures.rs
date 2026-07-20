//! External harness failure-path tests: timeout, non-2xx, malformed JSON,
//! ok=false, schema violation, secret-field scanning, request integrity.
//! All tests use real Runtime::deliver (no hand-built Run).
//! Reuses helpers from sibling module external_harness_runtime.

use super::external_harness_runtime::{
    config, event_with_time_grant, harness_200, register_and_enable, start_responder,
    CaptureToolsLlm,
};
use anyhow::Result;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
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

// ── Helper: assert round-2 follow-up has a failed ToolResult ──
fn assert_round2_failed(captured: &[serde_json::Value]) {
    assert!(captured.len() == 2, "LLM called twice");
    assert_eq!(captured[1]["follow_up_count"].as_u64().unwrap_or(0), 1);
    assert_eq!(
        captured[1]["follow_ups"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        1,
        "round-2 follow_ups.len() == 1"
    );
    let fu = &captured[1]["follow_ups"][0];
    assert_eq!(
        fu["provider_turn"]["canonical_operation"], "external.time_now",
        "assistant tool call operation"
    );
    assert_eq!(
        fu["provider_turn"]["provider_tool_call_id"], "cr",
        "provider tool call id matches"
    );
    let rc = fu["result_content"].as_str().unwrap_or("");
    assert!(
        rc.contains("execution_failed"),
        "ToolResult must contain execution_failed; got: {rc}"
    );
    assert!(
        rc.contains("error_category:"),
        "ToolResult must contain error_category; got: {rc}"
    );
}

// ═══ Secret fields — journal scan ═══

#[test]
fn harness_secret_fields_are_not_in_journal() -> Result<()> {
    let body = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1},
        "SECRET_TOKEN_MARKER":"x","fake_receipt":{"s":"ok"},"fake_journal_event":{"kind":"RC"},"fake_status":"ok","external_ref":"/private/path",
        "fake_decision_id":"dec_bogus","fake_occurred_at":"2026-01-01T00:00:00Z"});
    let (ep, _) = start_responder(&harness_200(&body.to_string()))?;
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: Arc::new(Mutex::new(Vec::new())),
            first: AtomicBool::new(true),
        },
    )
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let s = serde_json::to_string(&j.events()?).unwrap_or_default();
    for f in &[
        "SECRET_TOKEN_MARKER",
        "/private/internal/path",
        "fake_receipt",
        "fake_journal_event",
        "fake_status",
        "fake_decision_id",
        "fake_occurred_at",
    ] {
        assert!(!s.contains(f), "leaked {f} in journal");
    }
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
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let ev = j.events()?;
    let jt = serde_json::to_string(&ev).unwrap_or_default();
    for &m in SM {
        assert!(!jt.contains(m), "journal leaked {m}");
    }
    for c in cap.lock().unwrap().iter() {
        if let Some(fus) = c["follow_ups"].as_array() {
            for fu in fus {
                if let Some(rc) = fu["result_content"].as_str() {
                    for &m in SM {
                        assert!(!rc.contains(m), "ToolResult leaked {m}");
                    }
                }
            }
        }
        let s = serde_json::to_string(c).unwrap_or_default();
        for &m in SM {
            assert!(!s.contains(m), "LlmInput leaked {m}");
        }
    }
    assert!(!o.output.contains("SECRET_TOKEN_MARKER"));
    Ok(())
}

// ═══ Request body integrity ═══

#[test]
fn harness_request_contains_no_internal_fields() -> Result<()> {
    let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let cb = captured.clone();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let ep = format!("http://127.0.0.1:{port}/execute");
    thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut b = [0u8; 4096];
            let n = s.read(&mut b).unwrap_or(0);
            *cb.lock().unwrap() = String::from_utf8_lossy(&b[..n]).to_string();
            let _=s.write_all(harness_200(r#"{"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1}}"#).as_bytes());
        }
    });
    thread::sleep(Duration::from_millis(50));
    let j = crate::journal::JournalStore::in_memory()?;
    let g = crate::gateway::Gateway::new(config());
    register_and_enable(&j, &g, &ep)?;
    super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: Arc::new(Mutex::new(Vec::new())),
            first: AtomicBool::new(true),
        },
    )
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let raw = captured.lock().unwrap();
    let bs = raw.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    let jb = &raw[bs..];
    let p: serde_json::Value =
        serde_json::from_str(jb).expect("harness request body must be valid JSON");
    assert!(p
        .get("arguments")
        .and_then(|a| a.get("session_id"))
        .is_none());
    assert!(!jb.contains("test-token"));
    assert!(!jb.contains("connector_token"));
    assert!(!jb.contains("ingress_payload"));
    assert!(!jb.contains("IngressEnvelope"));
    Ok(())
}

// ═══ Schema violation ═══

#[test]
fn harness_extra_fields_in_result_cause_schema_violation() -> Result<()> {
    let b = json!({"protocol_version":"external-harness-v1","ok":true,"result":{"iso":"x","epoch_ms":1,"extra":"bad"}});
    let (ep, _) = start_responder(&harness_200(&b.to_string()))?;
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
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let r: Vec<_> = j
        .events()?
        .into_iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(
        r[0].payload["output"]["error_category"],
        "output_schema_violation"
    );
    let caps = cap.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert_round2_failed(&caps);
    Ok(())
}

// ═══ ok=false ═══

#[test]
fn harness_ok_false_through_runtime_records_failed() -> Result<()> {
    let b =
        json!({"protocol_version":"external-harness-v1","ok":false,"error_code":"rate_limited"});
    let (ep, _) = start_responder(&harness_200(&b.to_string()))?;
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
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let r: Vec<_> = j
        .events()?
        .into_iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(
        r[0].payload["output"]["error_category"],
        "external_infrastructure_failure"
    );
    let caps = cap.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert_round2_failed(&caps);
    Ok(())
}

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
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
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
    let caps = cap.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert_round2_failed(&caps);
    let jt = serde_json::to_string(&ev).unwrap_or_default();
    assert!(!jt.contains("{not valid json}"));
    assert!(!jt.contains("{not valid"));
    Ok(())
}

// ═══ Timeout — fast test (short harness_read_timeout_ms) ═══

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
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(r[0].payload["output"]["error_category"], "timeout");
    assert_eq!(
        ev.iter()
            .filter(
                |e| e.kind == crate::domain::JournalEventKind::ReceiptReceived
                    && e.payload["status"] == "Succeeded"
            )
            .count(),
        0
    );
    // Prove round-2 failed ToolResult.
    let caps = cap.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert_round2_failed(&caps);
    // Leak scan: raw error text must not appear.
    let jt = serde_json::to_string(&ev).unwrap_or_default();
    assert!(!jt.contains("WouldBlock"));
    assert!(!jt.contains("EAGAIN"));
    assert!(!jt.contains("timed out"));
    assert!(!jt.contains("socket"));
    for c in caps.iter() {
        let s = serde_json::to_string(c).unwrap_or_default();
        assert!(!s.contains("timed out"));
        assert!(!s.contains("WouldBlock"));
    }
    Ok(())
}

// ═══ Non-2xx through Runtime (500) — with round-2 failed ToolResult ═══

#[test]
fn harness_non_2xx_through_runtime_records_failed_with_round2() -> Result<()> {
    let (ep, _) = start_responder(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror",
    )?;
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
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let ev = j.events()?;
    let r: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    assert_eq!(r[0].payload["output"]["error_category"], "http_error");
    assert_eq!(r[0].payload["output"]["http_code"], 500);
    assert_eq!(
        ev.iter()
            .filter(
                |e| e.kind == crate::domain::JournalEventKind::ReceiptReceived
                    && e.payload["status"] == "Succeeded"
            )
            .count(),
        0
    );
    let caps = cap.lock().unwrap();
    assert_eq!(caps.len(), 2);
    assert_round2_failed(&caps);
    // Leak scan: raw response body and secret markers must not appear.
    let jt = serde_json::to_string(&ev).unwrap_or_default();
    assert!(!jt.contains("Internal Server Error"));
    for &m in SM {
        assert!(!jt.contains(m));
    }
    Ok(())
}

// ═══ Timeout — original 10s regression test ═══

#[test]
fn harness_timeout_through_runtime_records_failed() -> Result<()> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    let p = l.local_addr()?.port();
    let ep = format!("http://127.0.0.1:{p}/execute");
    thread::spawn(move || {
        if let Ok((_, _)) = l.accept() {
            thread::sleep(Duration::from_millis(500));
        }
    });
    thread::sleep(Duration::from_millis(50));
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
    .deliver(&j, &g, event_with_time_grant(&j, &g))?;
    let r: Vec<_> = j
        .events()?
        .into_iter()
        .filter(|e| e.kind == crate::domain::JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].payload["status"], "Failed");
    Ok(())
}
