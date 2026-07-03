//! Concurrency tests for capability proposal activation. Two independent
//! JournalStore instances (separate SQLite connections) race the atomic
//! `activate_proposal_atomic`. SQLite `BEGIN IMMEDIATE` serializes the writers,
//! so exactly one activation succeeds and the loser observes either
//! `proposal_not_pending` (the winner already committed) or
//! `registry_activation_conflict` (the CAS on version failed).

use crate::capabilities::store::ContentStore;
use crate::domain::capability_change::*;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::registry::snapshot::OperationSpec;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

fn config() -> crate::config::KernelConfig {
    crate::config::KernelConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        data_dir: std::path::PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: std::path::PathBuf::from("."),
        kernel_port: 0,
        connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        capability_submit_token: None,
        capability_decision_token: None,
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
    }
}

const PROBE_OP: &str = "external.capability_probe";
const ENDPOINT: &str = "http://127.0.0.1:18988/probe";

/// A content store + valid proposal blobs for the probe operation.
fn fresh_store_with_blobs() -> Result<ContentStore> {
    let dir = std::env::temp_dir().join(format!(
        "cap_conc_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(ContentStore::new(dir.join("store")))
}

/// Write the three blobs and return (artifact_digest, manifest_digest,
/// evidence_digest, manifest_id) plus the manifest for spec construction.
fn write_blobs(store: &ContentStore) -> Result<(String, String, String, HarnessManifest)> {
    let artifact_digest = store.store(b"#!/bin/sh\necho concurrent probe\n")?;
    let _evidence_digest = store.store(br#"{"attestation":"concurrent"}"#)?;
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "concurrent_probe_harness".into(),
        artifact_digest: artifact_digest.as_str().into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: ENDPOINT.into(),
        operation_name: PROBE_OP.into(),
        description: "concurrent probe".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"}},"required":["status","ok"],"additionalProperties":false}),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest = store.store(&manifest_bytes)?;
    Ok((
        artifact_digest.as_str().to_string(),
        manifest_digest.as_str().to_string(),
        _evidence_digest.as_str().to_string(),
        manifest,
    ))
}

/// Build a proposal body for the probe op referencing the given digests.
fn proposal_body(art: &str, man: &str, ev: &str) -> Value {
    json!({
        "target_agent_id": "main",
        "artifact_ref": "a", "artifact_digest": art,
        "manifest_ref": "m", "manifest_digest": man,
        "evidence_ref": "e", "evidence_digest": ev,
        "requested_operations": [PROBE_OP],
        "risk_summary": "concurrent probe",
    })
}

/// Count journal events of a given kind on a given connection.
fn count_events(journal: &JournalStore, kind: JournalEventKind) -> usize {
    journal
        .events()
        .unwrap_or_default()
        .iter()
        .filter(|e| e.kind == kind)
        .count()
}

fn registry_version(journal: &JournalStore) -> i64 {
    let conn = journal.conn.lock().unwrap();
    conn.query_row(
        "SELECT version FROM registry_state WHERE singleton_id = 1",
        [],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// The new operation specs to activate: current snapshot ops + the probe.
fn new_specs(journal: &JournalStore, manifest: &HarnessManifest) -> Result<Vec<OperationSpec>> {
    let cur = journal.current_registry_snapshot_id()?;
    let snap = journal.load_registry_snapshot(&cur)?;
    let mut specs: Vec<OperationSpec> = snap.operations.iter().cloned().collect();
    specs.push(OperationSpec {
        name: manifest.operation_name.clone(),
        risk: crate::registry::snapshot::Risk::ReadOnly,
        description: manifest.description.clone(),
        parameters: manifest.input_schema.clone(),
        idempotent: manifest.idempotent,
        binding_kind: crate::registry::snapshot::BindingKind::External,
        binding_key: manifest.manifest_id.clone(),
    });
    Ok(specs)
}

#[test]
fn concurrent_approved_decisions_activate_exactly_once() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "cap_conc_db_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("kernel.sqlite");

    // 1. Initialize the DB + registry on a setup store, then drop it.
    let j_setup = JournalStore::open(&db_path)?;
    j_setup.initialize_registry()?;
    let s0 = j_setup.current_registry_snapshot_id()?;

    // 2. Write the blobs and create the Pending proposal.
    let store = fresh_store_with_blobs()?;
    let (art, man, ev, manifest) = write_blobs(&store)?;
    let gw = Gateway::new(config());
    let body = proposal_body(&art, &man, &ev);
    let resp = crate::server::capability_routes::handle_submit_proposal(
        &j_setup,
        &gw,
        &body,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let pid = resp.proposal_id;
    let expected = j_setup.current_registry_snapshot_id()?;
    drop(j_setup);

    // 3. Two independent JournalStores open the same file. Both observe the
    //    proposal is Pending before racing activate_proposal_atomic.
    let store_a = Arc::new(JournalStore::open(&db_path)?);
    store_a.initialize_registry()?;
    let store_b = Arc::new(JournalStore::open(&db_path)?);
    store_b.initialize_registry()?;

    // Both confirm the proposal is still PendingApproval (the precondition).
    assert_eq!(
        store_a.load_proposal(&pid)?.unwrap().status,
        ProposalStatus::PendingApproval
    );
    assert_eq!(
        store_b.load_proposal(&pid)?.unwrap().status,
        ProposalStatus::PendingApproval
    );

    let v0 = registry_version(&store_a);
    let specs_a = new_specs(&store_a, &manifest)?;
    let specs_b = new_specs(&store_b, &manifest)?;
    let proposal_a = store_a.load_proposal(&pid)?.unwrap();
    let proposal_b = store_b.load_proposal(&pid)?.unwrap();

    // A barrier so both threads release together and race the transaction.
    let barrier = Arc::new(Barrier::new(2));
    let success_count = Arc::new(AtomicUsize::new(0));
    let conflict_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for (store, proposal, specs, expected) in [
        (store_a.clone(), proposal_a, specs_a, expected.clone()),
        (store_b.clone(), proposal_b, specs_b, expected.clone()),
    ] {
        let barrier = barrier.clone();
        let success_count = success_count.clone();
        let conflict_count = conflict_count.clone();
        let pid_local = pid.clone();
        handles.push(thread::spawn(move || {
            // Wait until both threads are ready, then race.
            barrier.wait();
            let res = store.activate_proposal_atomic(
                &proposal,
                "approval_workflow",
                specs,
                &expected,
                &format!("activation:{pid_local}"),
                None,
                &crate::domain::AgentId("main".to_string()),
            );
            match res {
                Ok(_) => {
                    success_count.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => {
                    let msg = e.to_string();
                    // The loser must fail with proposal_not_pending or a CAS
                    // conflict — never silently succeed.
                    assert!(
                        msg.contains("proposal_not_pending")
                            || msg.contains("registry_activation_conflict"),
                        "unexpected loser error: {msg}"
                    );
                    conflict_count.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Exactly one activation succeeded; exactly one failed.
    assert_eq!(
        success_count.load(Ordering::SeqCst),
        1,
        "exactly one success"
    );
    assert_eq!(
        conflict_count.load(Ordering::SeqCst),
        1,
        "exactly one failure"
    );

    // Re-open to read the committed final state from a single connection.
    let j_final = JournalStore::open(&db_path)?;
    j_final.initialize_registry()?;

    let p = j_final.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::Activated);
    let activated = p.activated_snapshot_id.clone().unwrap();

    // registry active == activated_snapshot_id (single, unique activation).
    assert_eq!(j_final.current_registry_snapshot_id()?, activated);
    assert_eq!(
        j_final.load_active_snapshot_from_state()?,
        Some(activated.clone())
    );

    // version increased by exactly 1.
    assert_eq!(registry_version(&j_final), v0 + 1);

    // Exactly one of each terminal event.
    assert_eq!(
        count_events(&j_final, JournalEventKind::CapabilityChangeActivated),
        1
    );
    assert_eq!(
        count_events(&j_final, JournalEventKind::RegistrySnapshotActivated),
        1
    );
    assert_eq!(
        count_events(&j_final, JournalEventKind::CapabilityChangeRejected),
        0
    );

    // The new snapshot exposes the probe.
    let snap = j_final.load_registry_snapshot(&activated)?;
    assert!(snap.lookup(PROBE_OP).is_some());
    assert_ne!(activated, s0);
    Ok(())
}

#[test]
fn approved_and_rejected_decisions_race_exactly_once() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "cap_race_db_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("kernel.sqlite");

    let j_setup = JournalStore::open(&db_path)?;
    j_setup.initialize_registry()?;
    let s0 = j_setup.current_registry_snapshot_id()?;

    let store = fresh_store_with_blobs()?;
    let (art, man, ev, manifest) = write_blobs(&store)?;
    let gw = Gateway::new(config());
    let body = proposal_body(&art, &man, &ev);
    let resp = crate::server::capability_routes::handle_submit_proposal(
        &j_setup,
        &gw,
        &body,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let pid = resp.proposal_id;
    let expected = j_setup.current_registry_snapshot_id()?;
    drop(j_setup);

    let store_a = Arc::new(JournalStore::open(&db_path)?);
    store_a.initialize_registry()?;
    let store_b = Arc::new(JournalStore::open(&db_path)?);
    store_b.initialize_registry()?;

    let v0 = registry_version(&store_a);
    let specs = new_specs(&store_a, &manifest)?;
    let proposal_a = store_a.load_proposal(&pid)?.unwrap();

    let barrier = Arc::new(Barrier::new(2));

    // Executor 1: approved — atomic activation.
    let h1 = {
        let barrier = barrier.clone();
        let store_a = store_a.clone();
        let expected = expected.clone();
        let pid_c = pid.clone();
        thread::spawn(move || -> Result<()> {
            barrier.wait();
            let res = store_a.activate_proposal_atomic(
                &proposal_a,
                "approval_workflow",
                specs,
                &expected,
                &format!("activation:{pid_c}"),
                None,
                &crate::domain::AgentId("main".to_string()),
            );
            if let Err(e) = res {
                let msg = e.to_string();
                // The loser of an approve/reject race either finds the proposal
                // already Rejected (proposal_not_pending) or loses the registry
                // CAS. Both are valid consistent outcomes — never a silent success.
                assert!(
                    msg.contains("proposal_not_pending")
                        || msg.contains("registry_activation_conflict"),
                    "unexpected approved-race error: {msg}"
                );
            }
            Ok(())
        })
    };

    // Executor 2: rejected — uses the atomic reject method (single tx).
    let h2 = {
        let barrier = barrier.clone();
        let store_b = store_b.clone();
        let pid_c = pid.clone();
        thread::spawn(move || -> Result<()> {
            barrier.wait();
            let result = store_b.reject_proposal_atomic(&pid_c, "approval_workflow", "rejected");
            if let Err(e) = result {
                // If the other executor activated first, reject is expected to
                // fail with proposal_not_pending — consistent outcome.
                let msg = e.to_string();
                assert!(
                    msg.contains("proposal_not_pending"),
                    "unexpected reject-race error: {msg}"
                );
            }
            Ok(())
        })
    };

    let _ = (h1.join().unwrap(), h2.join().unwrap());

    // Read the final committed state.
    let j_final = JournalStore::open(&db_path)?;
    j_final.initialize_registry()?;
    let p = j_final.load_proposal(&pid)?.unwrap();

    let n_activated = count_events(&j_final, JournalEventKind::CapabilityChangeActivated);
    let n_snap = count_events(&j_final, JournalEventKind::RegistrySnapshotActivated);
    let n_rejected = count_events(&j_final, JournalEventKind::CapabilityChangeRejected);

    match p.status {
        ProposalStatus::Activated => {
            // Consistent Activated: CapActivated + SnapActivated present, no CapRejected.
            assert_eq!(
                n_activated, 1,
                "Activated state must have exactly one CapActivated"
            );
            assert_eq!(
                n_snap, 1,
                "Activated state must have exactly one SnapActivated"
            );
            assert_eq!(n_rejected, 0, "Activated state must have no CapRejected");
            // registry moved forward by exactly 1.
            assert_eq!(registry_version(&j_final), v0 + 1);
            assert_ne!(j_final.current_registry_snapshot_id()?, s0);
        }
        ProposalStatus::Rejected => {
            // Consistent Rejected: CapRejected present, snapshot unchanged,
            // no CapActivated, no SnapActivated.
            assert_eq!(
                n_rejected, 1,
                "Rejected state must have exactly one CapRejected"
            );
            assert_eq!(n_activated, 0, "Rejected state must have no CapActivated");
            assert_eq!(n_snap, 0, "Rejected state must have no SnapActivated");
            assert_eq!(j_final.current_registry_snapshot_id()?, s0);
            assert_eq!(registry_version(&j_final), v0);
        }
        other => panic!("inconsistent terminal state: {other:?} — must be Activated or Rejected"),
    }

    // Forbidden: never Rejected-but-activated, never Activated-with-Rejected.
    assert!(
        !matches!(p.status, ProposalStatus::Activated) || n_rejected == 0,
        "forbidden: Activated but a Rejected event exists"
    );
    assert!(
        !matches!(p.status, ProposalStatus::Rejected)
            || j_final.current_registry_snapshot_id()? == s0,
        "forbidden: Rejected but the active snapshot moved"
    );
    Ok(())
}
