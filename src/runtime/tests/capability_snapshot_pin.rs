//! current Run / future Run Snapshot proof: activating a capability via the
//! real `handle_decision` → `activate_proposal_atomic` path affects ONLY future
//! Runs. A Run pinned to S0 keeps S0 (and its tool set) even after the registry
//! advances to S1; only a subsequent Run picks up S1 + the new probe capability.
//!
//! Uses the real `Runtime::deliver` → `get_or_create_session` → `create_run`
//! path (Run creation assigns `registry_snapshot_id` from the active snapshot
//! at creation time). No test manually rewrites `registry_snapshot_id`.

use super::external_harness_runtime::config;
use crate::capabilities::store::ContentStore;
use crate::domain::capability_change::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::{handle_decision, handle_submit_proposal};
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const PROBE_OP: &str = "external.capability_probe";
const ENDPOINT: &str = "http://127.0.0.1:18987/probe";

/// A minimal LLM that returns a plain assistant reply (no tool call) on every
/// round, and captures the provider_tools list it was offered.
struct CaptureToolsLlm {
    captured: Arc<Mutex<Vec<Value>>>,
}

impl crate::llm::LlmClient for CaptureToolsLlm {
    fn complete(&self, input: crate::llm::LlmInput) -> anyhow::Result<crate::llm::LlmOutput> {
        self.captured
            .lock()
            .unwrap()
            .push(json!({"provider_tools": input.provider_tools}));
        Ok(crate::llm::LlmOutput {
            provider: "t".into(),
            model: "t".into(),
            content: "ok".into(),
            journal_payload: json!({"s":"ok"}),
            tool_call: crate::llm::ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

/// Write artifact/manifest/evidence blobs and submit a valid probe proposal.
/// Returns the proposal id.
fn submit_probe(journal: &JournalStore, gateway: &Gateway, store: &ContentStore) -> Result<String> {
    let artifact_digest = store.store(b"#!/bin/sh\necho snapshot-pin probe\n")?;
    let evidence_digest = store.store(br#"{"attestation":"snapshot-pin"}"#)?;
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "snapshot_pin_probe_harness".into(),
        artifact_digest: artifact_digest.as_str().into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ENDPOINT.into(),
        operation_name: PROBE_OP.into(),
        description: "Snapshot-pin probe".into(),
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
        "artifact_ref": "a", "artifact_digest": artifact_digest.as_str(),
        "manifest_ref": "m", "manifest_digest": manifest_digest.as_str(),
        "evidence_ref": "e", "evidence_digest": evidence_digest.as_str(),
        "requested_operations": [PROBE_OP],
        "risk_summary": "snapshot-pin probe",
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

#[test]
fn capability_decision_activation_affects_only_future_runs() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "snap_pin_{}_{}",
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

    // 1. S0 = current snapshot, which does NOT contain the probe.
    let s0 = journal.current_registry_snapshot_id()?;
    let snap0 = journal.load_registry_snapshot(&s0)?;
    assert!(
        snap0.lookup(PROBE_OP).is_none(),
        "S0 must not contain {PROBE_OP} before activation"
    );

    // 2. Create a real Session + Run R0 via the production Runtime path.
    let captured0 = Arc::new(Mutex::new(Vec::new()));
    let rt0 = super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: captured0.clone(),
        },
    );
    let event0 = gateway.validate_ingress(&journal, gateway.cli_ingress("hello".into())?)?;
    let outcome0 = rt0.deliver(&journal, &gateway, event0)?;
    let r0 = outcome0.run_id.clone();

    // 3. R0 pinned to S0; its Provider tools lack the probe.
    let run0 = journal.run(&r0)?.expect("R0 exists");
    assert_eq!(run0.registry_snapshot_id, s0, "R0 must pin to S0");
    let caps0 = captured0.lock().unwrap();
    assert!(!caps0.is_empty(), "R0 captured at least one LLM round");
    let r0_tools: Vec<&str> = caps0[0]["provider_tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !r0_tools.contains(&PROBE_OP),
        "R0 Provider tools must not contain {PROBE_OP}; got {r0_tools:?}"
    );

    // 4. Submit the probe proposal and approve it via the real decision path.
    let pid = submit_probe(&journal, &gateway, &store)?;
    // The decision body carries the real digests stored on the proposal.
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
    assert_ne!(s1, s0, "activation must produce a new snapshot");

    // 5. registry active is now S1; the proposal is Activated.
    assert_eq!(journal.current_registry_snapshot_id()?, s1);
    let p_final = journal.load_proposal(&pid)?.unwrap();
    assert_eq!(p_final.status, ProposalStatus::Activated);
    assert_eq!(p_final.activated_snapshot_id.as_deref(), Some(s1.as_str()));

    // 6. R0 is STILL pinned to S0 — its snapshot was not rewritten.
    let run0_after = journal.run(&r0)?.expect("R0 still exists");
    assert_eq!(
        run0_after.registry_snapshot_id, s0,
        "R0 must remain pinned to S0 after activation"
    );

    // 7. A new Run R1 picks up S1 and exposes the probe in its tools.
    let captured1 = Arc::new(Mutex::new(Vec::new()));
    let rt1 = super::Runtime::new(
        config(),
        CaptureToolsLlm {
            captured: captured1.clone(),
        },
    );
    let event1 = gateway.validate_ingress(&journal, gateway.cli_ingress("hello again".into())?)?;
    let outcome1 = rt1.deliver(&journal, &gateway, event1)?;
    let r1 = outcome1.run_id.clone();

    let run1 = journal.run(&r1)?.expect("R1 exists");
    assert_eq!(run1.registry_snapshot_id, s1, "R1 must pin to S1");
    let caps1 = captured1.lock().unwrap();
    assert!(!caps1.is_empty(), "R1 captured at least one LLM round");
    let r1_tools: Vec<&str> = caps1[0]["provider_tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        r1_tools.contains(&PROBE_OP),
        "R1 Provider tools must contain {PROBE_OP}; got {r1_tools:?}"
    );
    assert!(
        run1.principal
            .grants
            .iter()
            .any(|g| g.operation == PROBE_OP),
        "R1 principal must be granted {PROBE_OP}"
    );

    // Final report values: real S0/R0/S1/R1 ids.
    eprintln!("PROOF S0={s0} R0={} S1={s1} R1={}", r0.0, r1.0);
    Ok(())
}
