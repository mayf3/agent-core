//! Schema-upgrade conflict tests:
//!  (1) two UNRELATED operation schema-upgrade proposals activated in sequence
//!      must merge into the snapshot (not last-write-wins);
//!  (2) two concurrent schema-upgrade proposals on the SAME operation+manifest
//!      must have exactly one winner and one stable stale/conflict loser.

use super::super::capability_routes_support::*;
use crate::capabilities::store::Sha256Digest;
use crate::domain::capability_change::ProposalStatus;
use crate::domain::*;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::{anyhow, Result};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

/// Load artifact bytes a setup stored, so an upgrade setup can re-store them.
fn artifact_bytes_of(setup: &ProposalSetup) -> Result<Vec<u8>> {
    let digest = Sha256Digest::parse(&setup.artifact_digest)?;
    Ok(setup.store.load(&digest)?)
}

// ── Section 2: two unrelated operation proposals, sequential activation ─────

/// Both `external.foo` and `external.bar` start from the same initial snapshot.
/// Approve+activate foo first, then bar. The final snapshot must contain BOTH
/// upgraded schemas (merge semantics, not last-write-wins), each activation
/// event must reference the correct proposal/decision/manifest, and bar must
/// NOT clobber foo.
#[test]
fn two_unrelated_schema_upgrades_merge_into_snapshot() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();

    // 1. Activate foo + bar (two separate operations) → snapshot S1.
    let foo_setup = ProposalSetup::build("external.foo", ENDPOINT, None)?;
    let bar_setup = ProposalSetup::build("external.bar", ENDPOINT, None)?;
    // bar_setup uses a separate store; both setups use the same artifact bytes
    // (the default artifact content) so a shared artifact_digest is consistent.
    let pid_foo = foo_setup.submit(&journal, &gw)?;
    handle_decision(
        &journal,
        &gw,
        &foo_setup.store,
        &pid_foo,
        &foo_setup.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let pid_bar = bar_setup.submit(&journal, &gw)?;
    handle_decision(
        &journal,
        &gw,
        &bar_setup.store,
        &pid_bar,
        &bar_setup.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let s1 = journal.current_registry_snapshot_id()?;
    let foo_old_id = foo_setup.manifest_id.clone();
    let bar_old_id = bar_setup.manifest_id.clone();

    // 2. Build schema-upgrade setups for foo and bar (different descriptions).
    let foo_old = journal
        .load_harness_manifest(&foo_old_id)?
        .ok_or_else(|| anyhow!("foo old manifest missing"))?;
    let bar_old = journal
        .load_harness_manifest(&bar_old_id)?
        .ok_or_else(|| anyhow!("bar old manifest missing"))?;
    let foo_art = artifact_bytes_of(&foo_setup)?;
    let bar_art = artifact_bytes_of(&bar_setup)?;

    let foo_up = SchemaUpgradeSetup::build(
        &foo_old,
        &foo_art,
        Some("Foo v2 (schema only)."),
        Some(
            json!({"type":"object","properties":{"foo_field":{"type":"string"}},"required":["foo_field"],"additionalProperties":false}),
        ),
        None,
        None,
    )?;
    let bar_up = SchemaUpgradeSetup::build(
        &bar_old,
        &bar_art,
        Some("Bar v2 (schema only)."),
        Some(
            json!({"type":"object","properties":{"bar_field":{"type":"integer"}},"required":["bar_field"],"additionalProperties":false}),
        ),
        None,
        None,
    )?;
    let foo_new_id = foo_up.manifest.manifest_id.clone();
    let bar_new_id = bar_up.manifest.manifest_id.clone();

    // 3. Approve + activate foo first.
    let pid_foo_up = foo_up.submit(&journal, &gw)?;
    let res_foo = handle_decision(
        &journal,
        &gw,
        &foo_up.store,
        &pid_foo_up,
        &foo_up.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let s2 = res_foo["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 4. Approve + activate bar — bar's proposal must be built from S1 still
    //    (expected_active_snapshot_id is captured at submit time). bar's
    //    activation must observe the LATEST snapshot and merge, not overwrite.
    let pid_bar_up = bar_up.submit(&journal, &gw)?;
    let res_bar = handle_decision(
        &journal,
        &gw,
        &bar_up.store,
        &pid_bar_up,
        &bar_up.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let s3 = res_bar["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(s3, s2);
    assert_ne!(s3, s1);

    // 5. Final snapshot contains BOTH upgraded schemas (merge, not overwrite).
    let snap = journal.load_registry_snapshot(&s3)?;
    let foo_spec = snap.lookup("external.foo").unwrap();
    let bar_spec = snap.lookup("external.bar").unwrap();
    assert_eq!(
        foo_spec.binding_key, foo_new_id,
        "foo must keep its new manifest"
    );
    assert_eq!(
        bar_spec.binding_key, bar_new_id,
        "bar must keep its new manifest"
    );
    // foo's schema reflects foo_up (not reverted by bar's activation).
    assert_eq!(
        foo_spec.parameters,
        json!({"type":"object","properties":{"foo_field":{"type":"string"}},"required":["foo_field"],"additionalProperties":false})
    );
    assert_eq!(
        bar_spec.parameters,
        json!({"type":"object","properties":{"bar_field":{"type":"integer"}},"required":["bar_field"],"additionalProperties":false})
    );

    // 6. Both activation events exist, each referencing the correct proposal/
    //    decision/manifest.
    let payloads = schema_upgrade_payloads(&journal);
    assert_eq!(payloads.len(), 2, "two schema_upgrade events");
    let foo_payload = payloads
        .iter()
        .find(|p| p["operation_name"] == "external.foo")
        .ok_or_else(|| anyhow!("missing foo payload"))?;
    let bar_payload = payloads
        .iter()
        .find(|p| p["operation_name"] == "external.bar")
        .ok_or_else(|| anyhow!("missing bar payload"))?;
    assert_eq!(foo_payload["old_manifest_id"], foo_old_id);
    assert_eq!(foo_payload["new_manifest_id"], foo_new_id);
    assert_eq!(foo_payload["proposal_id"], pid_foo_up);
    assert_eq!(
        foo_payload["decision_id"],
        format!("schema_upgrade:{pid_foo_up}")
    );
    assert_eq!(bar_payload["old_manifest_id"], bar_old_id);
    assert_eq!(bar_payload["new_manifest_id"], bar_new_id);
    assert_eq!(bar_payload["proposal_id"], pid_bar_up);

    // 7. No unrelated operation lost; both old manifests preserved.
    assert!(manifest_exists(&journal, &foo_old_id));
    assert!(manifest_exists(&journal, &bar_old_id));
    assert!(manifest_exists(&journal, &foo_new_id));
    assert!(manifest_exists(&journal, &bar_new_id));
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

// ── Section 3: same-target concurrent conflict ─────────────────────────────

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
        coding_harness_api_url: "http://127.0.0.1:7200".into(),
        coding_harness_artifact_digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
    }
}

/// Two independent SQLite connections/transactions race two schema-upgrade
/// proposals on the SAME operation and SAME old manifest. SQLite BEGIN
/// IMMEDIATE serializes the writers: exactly one upgrade activates and the
/// other must fail stably (stale/conflict). No partial writes; the registry
/// references exactly one new manifest; the journal hash chain stays valid.
#[test]
fn concurrent_schema_upgrades_on_same_target_resolve_exactly_once() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "cap_upgrade_conc_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("kernel.sqlite");

    // 1. Initialize DB + registry, then activate the probe so an old manifest
    //    exists to upgrade from.
    let j_setup = JournalStore::open(&db_path)?;
    j_setup.initialize_registry()?;
    let gw = crate::gateway::Gateway::new(config());
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid_create = setup.submit(&j_setup, &gw)?;
    handle_decision(
        &j_setup,
        &gw,
        &setup.store,
        &pid_create,
        &setup.approved_body(),
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let _s_after_create = j_setup.current_registry_snapshot_id()?;
    let old_manifest_id = setup.manifest_id.clone();
    let old_manifest = j_setup
        .load_harness_manifest(&old_manifest_id)?
        .ok_or_else(|| anyhow!("old manifest missing"))?;
    let art_bytes = {
        let digest = Sha256Digest::parse(&setup.artifact_digest)?;
        setup.store.load(&digest)?
    };
    let v0 = {
        let conn = j_setup.conn.lock().unwrap();
        conn.query_row(
            "SELECT version FROM registry_state WHERE singleton_id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )?
    };

    // 2. Build TWO distinct schema-upgrade manifests targeting the SAME old
    //    manifest (different descriptions → different manifest_ids). Each gets
    //    its own Pending proposal persisted to the (still single) DB.
    let up_a = SchemaUpgradeSetup::build(
        &old_manifest,
        &art_bytes,
        Some("Probe upgrade A (concurrent)."),
        Some(
            json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"],"additionalProperties":false}),
        ),
        None,
        None,
    )?;
    let up_b = SchemaUpgradeSetup::build(
        &old_manifest,
        &art_bytes,
        Some("Probe upgrade B (concurrent)."),
        Some(
            json!({"type":"object","properties":{"b":{"type":"integer"}},"required":["b"],"additionalProperties":false}),
        ),
        None,
        None,
    )?;
    let new_id_a = up_a.manifest.manifest_id.clone();
    let new_id_b = up_b.manifest.manifest_id.clone();
    assert_ne!(
        new_id_a, new_id_b,
        "two upgrades must produce distinct manifests"
    );

    let pid_a = up_a.submit(&j_setup, &gw)?;
    let pid_b = up_b.submit(&j_setup, &gw)?;
    let proposal_a = j_setup.load_proposal(&pid_a)?.unwrap();
    let proposal_b = j_setup.load_proposal(&pid_b)?.unwrap();
    drop(j_setup);

    // 3. Two independent JournalStores open the same file. Both observe both
    //    proposals Pending before racing activate_schema_upgrade_atomic.
    let store_a = Arc::new(JournalStore::open(&db_path)?);
    store_a.initialize_registry()?;
    let store_b = Arc::new(JournalStore::open(&db_path)?);
    store_b.initialize_registry()?;
    for s in [&store_a, &store_b] {
        assert_eq!(
            s.load_proposal(&pid_a)?.unwrap().status,
            ProposalStatus::PendingApproval
        );
        assert_eq!(
            s.load_proposal(&pid_b)?.unwrap().status,
            ProposalStatus::PendingApproval
        );
    }

    let barrier = Arc::new(Barrier::new(2));
    let success = Arc::new(AtomicUsize::new(0));
    let conflict = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    let cases = [
        (
            store_a.clone(),
            proposal_a,
            up_a.manifest.clone(),
            pid_a.clone(),
        ),
        (
            store_b.clone(),
            proposal_b,
            up_b.manifest.clone(),
            pid_b.clone(),
        ),
    ];
    for (store, proposal, manifest, pid_local) in cases {
        let barrier = barrier.clone();
        let success = success.clone();
        let conflict = conflict.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let decision_id = format!("schema_upgrade:{pid_local}");
            let res = store.activate_schema_upgrade_atomic(
                &proposal,
                "approval_workflow",
                &decision_id,
                &manifest,
                &AgentId("main".to_string()),
            );
            match res {
                Ok(_) => {
                    success.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => {
                    let msg = e.to_string();
                    // The loser must fail stably: stale snapshot, target
                    // changed, or a CAS conflict. Never silently succeed.
                    assert!(
                        msg.contains("stale_expected_snapshot")
                            || msg.contains("target_operation_changed")
                            || msg.contains("registry_activation_conflict")
                            || msg.contains("proposal_not_pending"),
                        "unexpected loser error: {msg}"
                    );
                    conflict.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        success.load(Ordering::SeqCst),
        1,
        "exactly one upgrade wins"
    );
    assert_eq!(
        conflict.load(Ordering::SeqCst),
        1,
        "exactly one upgrade loses"
    );

    // 4. Re-open and read committed final state from a single connection.
    let j_final = JournalStore::open(&db_path)?;
    j_final.initialize_registry()?;

    // Both proposals: exactly one Activated, the other still PendingApproval
    // (the loser's transaction rolled back fully — no partial write).
    let pa = j_final.load_proposal(&pid_a)?.unwrap();
    let pb = j_final.load_proposal(&pid_b)?.unwrap();
    let activated_a = pa.status == ProposalStatus::Activated;
    let activated_b = pb.status == ProposalStatus::Activated;
    assert!(activated_a ^ activated_b, "exactly one proposal Activated");
    let winner_manifest_id = if activated_a {
        new_id_a.clone()
    } else {
        new_id_b.clone()
    };
    let loser_pid = if activated_a { &pid_b } else { &pid_a };
    let loser = j_final.load_proposal(loser_pid)?.unwrap();
    assert_eq!(
        loser.status,
        ProposalStatus::PendingApproval,
        "loser must remain PendingApproval (no partial write)"
    );

    // 5. Registry references exactly one new manifest (the winner's).
    let active = j_final.current_registry_snapshot_id()?;
    let snap = j_final.load_registry_snapshot(&active)?;
    assert_eq!(
        snap.lookup(PROBE_OP).unwrap().binding_key,
        winner_manifest_id
    );
    assert_eq!(
        manifest_count_for_operation(&j_final, PROBE_OP),
        2,
        "old + exactly one new manifest row"
    );

    // 6. Exactly one successful schema_upgrade event in the journal.
    assert_eq!(schema_upgrade_payloads(&j_final).len(), 1);

    // 7. Version advanced by exactly one.
    let v_final = {
        let conn = j_final.conn.lock().unwrap();
        conn.query_row(
            "SELECT version FROM registry_state WHERE singleton_id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )?
    };
    assert_eq!(v_final, v0 + 1);

    // 8. Hash chain and DB consistency hold.
    assert!(j_final.verify_hash_chain()?);
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
