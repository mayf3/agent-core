//! Reopen consistency — an activated proposal + registry survive a full Kernel
//! restart (close all JournalStores, reopen the same SQLite file, re-init).

use super::capability_probe_e2e::submit_probe;
use super::external_harness_runtime::config;
use crate::capabilities::store::ContentStore;
use crate::domain::capability_change::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::Result;
use serde_json::json;

const PROBE_OP: &str = "external.capability_probe";

#[test]
fn activated_proposal_and_registry_survive_reopen() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "probe_reopen_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("kernel.sqlite");
    let store = ContentStore::new(dir.join("store"));

    // Use a localhost endpoint that's not actually contacted (no Run runs).
    let endpoint = "http://127.0.0.1:18986/probe";

    // Open, init, submit + approve.
    let j1 = JournalStore::open(&db_path)?;
    j1.initialize_registry()?;
    let gw = Gateway::new(config());
    let pid = submit_probe(&j1, &gw, &store, endpoint)?;
    let proposal_before = j1.load_proposal(&pid)?.unwrap();
    let dec = json!({
        "decision": "approved",
        "artifact_digest": proposal_before.artifact_digest,
        "manifest_digest": proposal_before.manifest_digest,
    });
    let result = handle_decision(
        &j1,
        &gw,
        &store,
        &pid,
        &dec,
        "approval_workflow",
        &crate::domain::AgentId("main".to_string()),
    )?;
    let activated = result["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let decided_by = "approval_workflow".to_string();
    let decided_at = j1.load_proposal(&pid)?.unwrap().decided_at;
    drop(j1);

    // Close ALL JournalStores, reopen the same SQLite file.
    let j2 = JournalStore::open(&db_path)?;
    j2.initialize_registry()?;

    // Proposal survived: status, snapshot id, digests, operations, decision
    // principal + time all preserved.
    let p = j2.load_proposal(&pid)?.unwrap();
    assert_eq!(p.status, ProposalStatus::Activated);
    assert_eq!(p.activated_snapshot_id.as_deref(), Some(activated.as_str()));
    assert_eq!(p.artifact_digest, proposal_before.artifact_digest);
    assert_eq!(p.manifest_digest, proposal_before.manifest_digest);
    assert_eq!(p.evidence_digest, proposal_before.evidence_digest);
    assert_eq!(p.requested_operations, proposal_before.requested_operations);
    assert_eq!(p.decided_by.as_deref(), Some(decided_by.as_str()));
    assert_eq!(p.decided_at, decided_at);

    // registry_state.active_snapshot_id == activated_snapshot_id.
    assert_eq!(
        j2.load_active_snapshot_from_state()?,
        Some(activated.clone())
    );
    assert_eq!(j2.current_registry_snapshot_id()?, activated);

    // The Registry Snapshot loads completely and contains the probe.
    let snap = j2.load_registry_snapshot(&activated)?;
    assert!(snap.lookup(PROBE_OP).is_some());
    assert_eq!(
        snap.lookup(PROBE_OP).unwrap().binding_kind,
        crate::registry::snapshot::BindingKind::External
    );
    Ok(())
}
