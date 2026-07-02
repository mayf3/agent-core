//! localhost probe complete Runtime E2E.
//!
//! The E2E test exercises the FULL capability-change-to-tool-loop chain with a
//! real localhost HTTP server: submit (submitter token) → self-decide rejected
//! → approval_workflow decides approved → Kernel re-verifies content via the
//! content store, parses+validates the manifest with the EXISTING validator,
//! binds operations exactly, and atomically activates → a new Run exposes the
//! probe tool → the model calls it → ToolCallIssued → InvocationProposed →
//! Gateway → InvocationApproved → ExternalHarnessAdapter → real localhost probe
//! → ReceiptReceived(Succeeded) → role=tool follow-up → final assistant reply.
//!
//! Reopen-consistency and atomic-rollback tests live in sibling modules
//! (capability_probe_reopen, capability_probe_rollback).

use super::external_harness_runtime::config;
use crate::capabilities::store::ContentStore;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::{handle_decision, handle_submit_proposal};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const PROBE_OP: &str = "external.capability_probe";

// ── localhost probe HTTP responder (counts requests) ───────────────────────

/// Start a real localhost HTTP server that responds to every probe POST with a
/// fixed 200 OK body, and counts how many requests it received. Returns the
/// endpoint URL and the request counter.
fn start_counting_responder(body: String) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://127.0.0.1:{port}/probe");
    let count = Arc::new(AtomicUsize::new(0));
    let count_t = count.clone();
    let body_t = body;
    thread::spawn(move || {
        // Serve up to a few requests so the loop is robust to retries; the
        // assertion is exactly 1.
        for _ in 0..8 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                count_t.fetch_add(1, Ordering::SeqCst);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_t.len(),
                    body_t
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });
    thread::sleep(Duration::from_millis(50));
    (endpoint, count)
}

/// Write artifact/manifest/evidence blobs for the probe and submit a proposal.
/// `pub(super)` so the reopen/rollback sibling test modules reuse it.
pub(super) fn submit_probe(
    journal: &JournalStore,
    gateway: &Gateway,
    store: &ContentStore,
    endpoint: &str,
) -> Result<String> {
    let artifact_digest = store.store(b"#!/bin/sh\necho capability probe\n")?;
    let evidence_digest = store.store(br#"{"attestation":"probe-build","signed_by":"ci"}"#)?;
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "capability_probe_harness".into(),
        artifact_digest: artifact_digest.as_str().into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: endpoint.into(),
        operation_name: PROBE_OP.into(),
        description: "Capability probe — read-only localhost health check.".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"}},"required":["status","ok"],"additionalProperties":false}),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest = store.store(&manifest_bytes)?;

    let body = json!({
        "target_agent_id": "main",
        "artifact_ref": "artifact.bin", "artifact_digest": artifact_digest.as_str(),
        "manifest_ref": "manifest.json", "manifest_digest": manifest_digest.as_str(),
        "evidence_ref": "evidence.json", "evidence_digest": evidence_digest.as_str(),
        "requested_operations": [PROBE_OP],
        "risk_summary": "read-only localhost probe",
    });
    let resp = handle_submit_proposal(
        journal,
        gateway,
        &body,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )?;
    Ok(resp.proposal_id)
}

/// An LLM that on round 1 emits a tool call to the probe (fixed id), and on
/// round 2 returns a final assistant reply echoing the probe result.
struct ProbeLoopLlm {
    captured: Arc<Mutex<Vec<Value>>>,
    first: AtomicBool,
}

impl crate::llm::LlmClient for ProbeLoopLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.captured.lock().unwrap().push(json!({
            "provider_tools": input.provider_tools,
            "follow_ups": super::external_harness_runtime::captured_follow_ups(&input),
            "follow_up_count": input.follow_ups.len(),
        }));
        if self.first.swap(false, Ordering::SeqCst) {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: String::new(),
                journal_payload: json!({"s":"ok"}),
                tool_call: crate::llm::ToolCallResult::Valid(crate::llm::ToolCall {
                    id: "probe_call_1".into(),
                    operation: PROBE_OP.into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(crate::llm::ProviderToolTurn {
                    endpoint: crate::llm::EndpointChoice::Primary,
                    provider_tool_call_id: "probe_call_1".into(),
                    wire_name: PROBE_OP.into(),
                    canonical_operation: PROBE_OP.into(),
                    arguments_json: "{}".into(),
                }),
            })
        } else {
            Ok(crate::llm::LlmOutput {
                provider: "t".into(),
                model: "t".into(),
                content: "probe completed: healthy".into(),
                journal_payload: json!({"s":"ok","c":"probe completed: healthy"}),
                tool_call: crate::llm::ToolCallResult::Absent,
                provider_turn: None,
            })
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// §1: Full localhost probe Runtime E2E
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn capability_probe_full_runtime_loop() -> Result<()> {
    // Probe returns a healthy status.
    let probe_body = json!({
        "protocol_version": "external-harness-v1",
        "ok": true,
        "result": {"status": "healthy", "ok": true}
    });
    let (endpoint, request_count) = start_counting_responder(probe_body.to_string());

    let dir = std::env::temp_dir().join(format!(
        "probe_e2e_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    let store = ContentStore::new(dir.join("store"));

    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config());
    let s0 = journal.current_registry_snapshot_id()?;
    let snap0 = journal.load_registry_snapshot(&s0)?;
    assert!(snap0.lookup(PROBE_OP).is_none(), "S0 has no probe");

    // ── Capability chain ──
    // 1. capability_submitter submits the proposal.
    let pid = submit_probe(&journal, &gateway, &store, &endpoint)?;

    // 2. The submitter tries to decide its own proposal → rejected
    //    (submitter_cannot_decide_own_proposal). The principal identity model
    //    keeps submit ≠ decide.
    let self_decide = json!({
        "decision": "approved",
        "artifact_digest": journal.load_proposal(&pid)?.unwrap().artifact_digest,
        "manifest_digest": journal.load_proposal(&pid)?.unwrap().manifest_digest,
    });
    let self_err = handle_decision(
        &journal,
        &gateway,
        &store,
        &pid,
        &self_decide,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        self_err.contains("submitter_cannot_decide_own_proposal"),
        "submitter must not self-decide; got: {self_err}"
    );

    // 3. approval_workflow decides approved → Kernel re-verifies content
    //    (ContentStore re-hash), parses + validates the manifest, binds the
    //    operation exactly, and activates atomically → S1.
    let proposal = journal.load_proposal(&pid)?.unwrap();
    let dec = json!({
        "decision": "approved",
        "artifact_digest": proposal.artifact_digest,
        "manifest_digest": proposal.manifest_digest,
    });
    let result = handle_decision(
        &journal,
        &gateway,
        &store,
        &pid,
        &dec,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )?;
    assert_eq!(result["status"], "Activated");
    let s1 = result["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(s1, s0);

    // ── Runtime tool loop on a NEW Run pinned to S1 ──
    let captured = Arc::new(Mutex::new(Vec::new()));
    let rt = super::Runtime::new(
        config(),
        ProbeLoopLlm {
            captured: captured.clone(),
            first: AtomicBool::new(true),
        },
    );
    let event = gateway.validate_ingress(&journal, gateway.cli_ingress("run probe".into())?)?;
    let outcome = rt.deliver(&journal, &gateway, event)?;
    let run_id = outcome.run_id.clone();
    let session_id = outcome.session_id.clone();

    // The new Run is pinned to S1 and has the probe grant + tool.
    let run = journal.run(&run_id)?.expect("run exists");
    assert_eq!(run.registry_snapshot_id, s1);
    assert!(run.principal.grants.iter().any(|g| g.operation == PROBE_OP));

    // localhost was hit exactly once.
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        1,
        "localhost probe must be hit exactly once"
    );

    // ── Provider/Invocation/Receipt chain ──
    let caps = captured.lock().unwrap();
    assert_eq!(caps.len(), 2, "LLM called twice (tool round + reply round)");

    // Round 1: probe tool offered.
    let r1_tools: Vec<&str> = caps[0]["provider_tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        r1_tools.contains(&PROBE_OP),
        "round 1 tools include {PROBE_OP}"
    );

    // Round 2: exactly one follow-up, same tool_call_id, succeeded status.
    let fu = caps[1]["follow_ups"]
        .as_array()
        .expect("round-2 follow_ups captured");
    assert_eq!(fu.len(), 1, "exactly one follow-up");
    let call = &fu[0]["provider_turn"];
    assert_eq!(call["canonical_operation"], PROBE_OP);
    assert_eq!(
        call["provider_tool_call_id"], "probe_call_1",
        "round-2 carries the round-1 tool_call_id"
    );
    let result_content = fu[0]["result_content"].as_str().unwrap_or("");
    assert!(
        result_content.contains("succeeded"),
        "tool result status succeeded; got {result_content}"
    );
    assert!(
        result_content.contains("healthy"),
        "tool result carries probe output; got {result_content}"
    );

    // The round-1 emitted tool_call_id == round-2 follow-up tool_call_id.
    assert_eq!(call["provider_tool_call_id"], "probe_call_1");

    // Final reply echoes the probe result.
    assert!(
        outcome.output.contains("healthy"),
        "final reply echoes probe output; got {}",
        outcome.output
    );

    // ── Journal events ──
    let ev = journal.events()?;
    let ti = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ToolCallIssued)
        .count();
    let ip = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::InvocationProposed)
        .count();
    let ia = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::InvocationApproved)
        .count();
    let receipts: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .collect();
    assert_eq!(ti, 1, "ToolCallIssued == 1");
    // The Runtime emits InvocationProposed/Approved for BOTH the tool call and
    // the final assistant reply invocation, so each is 2 (tool + reply) —
    // matching the proven external-harness E2E in external_harness_runtime.rs.
    // The probe-specific invariant is exactly one tool Receipt.
    assert_eq!(ip, 2, "InvocationProposed == 2 (tool + reply)");
    assert_eq!(ia, 2, "InvocationApproved == 2 (tool + reply)");
    assert_eq!(
        receipts.len(),
        1,
        "ReceiptReceived == 1 (exactly one probe)"
    );
    assert_eq!(receipts[0].payload["status"], "Succeeded");
    assert_eq!(receipts[0].payload["output"]["status"], "healthy");
    assert_eq!(receipts[0].payload["output"]["ok"], true);

    // The Receipt's invocation_id matches the tool-call
    // InvocationProposed/Approved. For InvocationProposed the invocation_id is
    // carried on the event's correlation_id (not the payload); for
    // InvocationApproved and ReceiptReceived it is also in the payload.
    let receipt_inv = receipts[0]
        .payload
        .get("invocation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(!receipt_inv.is_empty(), "Receipt carries an invocation_id");
    let proposed_match = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::InvocationProposed)
        .any(|e| e.correlation_id.as_deref() == Some(receipt_inv));
    let approved_match = ev
        .iter()
        .filter(|e| e.kind == JournalEventKind::InvocationApproved)
        .any(|e| e.correlation_id.as_deref() == Some(receipt_inv));
    assert!(
        proposed_match,
        "tool InvocationProposed correlation_id == Receipt invocation_id"
    );
    assert!(
        approved_match,
        "tool InvocationApproved invocation_id == Receipt invocation_id"
    );

    // run_id / session_id are correct.
    assert_eq!(
        receipts[0].run_id.as_ref().map(|r| r.0.as_str()),
        Some(run_id.0.as_str())
    );
    assert_eq!(
        receipts[0].session_id.as_ref().map(|s| s.0.as_str()),
        Some(session_id.0.as_str())
    );

    eprintln!(
        "PROOF localhost_requests=1 S0={s0} S1={s1} run={}",
        run_id.0
    );
    Ok(())
}
