//! Atomic activation rollback — when a journal event write fails mid-transaction,
//! the entire `activate_proposal_atomic` transaction rolls back: no new snapshot,
//! no version bump, no proposal status change, no half-completed terminal events.

use super::external_harness_runtime::config;
use crate::capabilities::store::ContentStore;
use crate::domain::capability_change::*;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_submit_proposal;
use anyhow::Result;
use serde_json::json;

const PROBE_OP: &str = "external.capability_probe";

/// Build the new operation specs (current ops + probe) for activation.
fn probe_specs(
    journal: &JournalStore,
    manifest: &HarnessManifest,
) -> Result<Vec<crate::registry::snapshot::OperationSpec>> {
    let cur = journal.current_registry_snapshot_id()?;
    let snap = journal.load_registry_snapshot(&cur)?;
    let mut specs: Vec<crate::registry::snapshot::OperationSpec> =
        snap.operations.iter().cloned().collect();
    specs.push(crate::registry::snapshot::OperationSpec {
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

/// Common setup: a Pending proposal + the manifest/specs ready to activate.
fn rollback_setup(
    journal: &JournalStore,
) -> Result<(
    String,
    HarnessManifest,
    Vec<crate::registry::snapshot::OperationSpec>,
    String,
)> {
    let gw = Gateway::new(config());
    let dir = std::env::temp_dir().join(format!(
        "probe_rollback_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    let _store = ContentStore::new(dir.join("store"));
    let artifact_digest = _store.store(b"#!/bin/sh\necho rollback probe\n")?;
    let _evidence_digest = _store.store(br#"{"attestation":"rollback"}"#)?;
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "rollback_probe_harness".into(),
        artifact_digest: artifact_digest.as_str().into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: "http://127.0.0.1:18985/probe".into(),
        operation_name: PROBE_OP.into(),
        description: "rollback probe".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"}},"required":["status","ok"],"additionalProperties":false}),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest = _store.store(&manifest_bytes)?;
    let body = json!({
        "target_agent_id": "main",
        "artifact_ref": "a", "artifact_digest": artifact_digest.as_str(),
        "manifest_ref": "m", "manifest_digest": manifest_digest.as_str(),
        "evidence_ref": "e", "evidence_digest": _evidence_digest.as_str(),
        "requested_operations": [PROBE_OP],
        "risk_summary": "rollback probe",
    });
    let resp = handle_submit_proposal(
        journal,
        &gw,
        &body,
        "capability_submitter",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let pid = resp.proposal_id;
    let proposal = journal.load_proposal(&pid)?.unwrap();
    let expected = proposal.expected_active_snapshot_id.clone();
    let specs = probe_specs(journal, &manifest)?;
    Ok((pid, manifest, specs, expected))
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

#[test]
fn registry_activation_event_failure_rolls_back_everything() -> Result<()> {
    // Use a file-backed DB so we can poison the journal_events table.
    let dir = std::env::temp_dir().join(format!(
        "rollback_snap_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("kernel.sqlite");

    let j = JournalStore::open(&db_path)?;
    j.initialize_registry()?;
    let s0 = j.current_registry_snapshot_id()?;
    let v0 = registry_version(&j);
    let (pid, manifest, specs, expected) = rollback_setup(&j)?;
    let proposal = j.load_proposal(&pid)?.unwrap();

    // Poison: make the RegistrySnapshotActivated event INSERT fail by dropping
    // the journal_events table. The transaction's INSERT (step 6) fails, so the
    // whole transaction rolls back.
    j.execute_sql_for_test("DROP TABLE journal_events")?;

    let res = j.activate_proposal_atomic(
        &proposal,
        "approval_workflow",
        specs,
        &expected,
        &format!("activation:{pid}"),
        None,
        &crate::domain::AgentId("main".to_string()),
    );
    assert!(res.is_err(), "activation must fail when event write fails");

    // Re-create the table so reads work (the drop was test-only poisoning).
    j.execute_sql_for_test(
        "CREATE TABLE journal_events (sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, run_id TEXT, session_id TEXT, correlation_id TEXT, kind TEXT NOT NULL, payload_json TEXT NOT NULL, previous_hash TEXT, hash TEXT NOT NULL, created_at TEXT NOT NULL)",
    )?;

    // Nothing changed: active snapshot == S0, version == v0.
    assert_eq!(j.current_registry_snapshot_id()?, s0);
    assert_eq!(registry_version(&j), v0);

    // The proposal did NOT reach Activated.
    let p = j.load_proposal(&pid)?.unwrap();
    assert_ne!(p.status, ProposalStatus::Activated);
    assert_eq!(p.status, ProposalStatus::PendingApproval);

    // No half-completed terminal events exist.
    let ev = j.events()?;
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::CapabilityChangeActivated)
            .count(),
        0
    );
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::RegistrySnapshotActivated)
            .count(),
        0
    );
    let _ = manifest;
    Ok(())
}

#[test]
fn capability_activation_event_failure_rolls_back_everything() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "rollback_cap_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("kernel.sqlite");

    let j = JournalStore::open(&db_path)?;
    j.initialize_registry()?;
    let s0 = j.current_registry_snapshot_id()?;
    let v0 = registry_version(&j);
    let (pid, manifest, specs, expected) = rollback_setup(&j)?;
    let proposal = j.load_proposal(&pid)?.unwrap();

    // Let the RegistrySnapshotActivated INSERT succeed but make the SECOND event
    // (CapabilityChangeActivated) fail via a CHECK constraint that rejects that
    // kind (applied via a temp-table swap). The whole transaction rolls back.
    j.execute_sql_for_test(
        "CREATE TABLE journal_events_ckpt AS SELECT * FROM journal_events WHERE 0",
    )?;
    j.execute_sql_for_test(
        "CREATE TABLE journal_events_new (sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, run_id TEXT, session_id TEXT, correlation_id TEXT, kind TEXT NOT NULL CHECK (kind <> 'CapabilityChangeActivated'), payload_json TEXT NOT NULL, previous_hash TEXT, hash TEXT NOT NULL, created_at TEXT NOT NULL)",
    )?;
    j.execute_sql_for_test("INSERT INTO journal_events_new SELECT * FROM journal_events")?;
    j.execute_sql_for_test("DROP TABLE journal_events")?;
    j.execute_sql_for_test("ALTER TABLE journal_events_new RENAME TO journal_events")?;

    let res = j.activate_proposal_atomic(
        &proposal,
        "approval_workflow",
        specs,
        &expected,
        &format!("activation:{pid}"),
        None,
        &crate::domain::AgentId("main".to_string()),
    );
    assert!(
        res.is_err(),
        "activation must fail when CapActivated write fails"
    );
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("CHECK"),
        "expected CHECK constraint failure; got: {err}"
    );

    // Rebuild a clean table for reads.
    j.execute_sql_for_test(
        "CREATE TABLE journal_events_clean (sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, run_id TEXT, session_id TEXT, correlation_id TEXT, kind TEXT NOT NULL, payload_json TEXT NOT NULL, previous_hash TEXT, hash TEXT NOT NULL, created_at TEXT NOT NULL)",
    )?;
    j.execute_sql_for_test("INSERT INTO journal_events_clean SELECT * FROM journal_events")?;
    j.execute_sql_for_test("DROP TABLE journal_events")?;
    j.execute_sql_for_test("ALTER TABLE journal_events_clean RENAME TO journal_events")?;

    // Nothing changed.
    assert_eq!(j.current_registry_snapshot_id()?, s0);
    assert_eq!(registry_version(&j), v0);
    let p = j.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::PendingApproval);
    let ev = j.events()?;
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::CapabilityChangeActivated)
            .count(),
        0
    );
    assert_eq!(
        ev.iter()
            .filter(|e| e.kind == JournalEventKind::RegistrySnapshotActivated)
            .count(),
        0,
        "no RegistrySnapshotActivated leaked from the rolled-back tx"
    );
    let _ = manifest;
    Ok(())
}
