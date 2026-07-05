//! Schema-upgrade tests for the seven canonical `external.coding_*`
//! operations. Each operation starts from a distinct old manifest in the same
//! initial snapshot; seven sequential schema upgrades must each produce a new
//! snapshot, preserve all prior upgrades (no rollback), keep every old manifest
//! queryable, and emit events with the exact schema_upgrade payload.

use super::super::capability_routes_support::*;
use crate::domain::*;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_decision;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

const CODING_ENDPOINT: &str = "http://127.0.0.1:7200/coding";
/// Placeholder artifact digest; replaced by the real stored digest before any
/// manifest is submitted.
const CODING_ARTIFACT_PLACEHOLDER: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The seven canonical coding operations, in upgrade order.
const CODING_OPS: &[&str] = &[
    "external.coding_task_submit",
    "external.coding_task_status",
    "external.coding_workspace_list",
    "external.coding_workspace_read",
    "external.coding_workspace_write",
    "external.coding_workspace_exec",
    "external.coding_capability_propose",
];

/// Build a distinct old manifest for `op_name` with a per-op input schema.
/// `variant` distinguishes old (0) vs upgraded (1) so manifest_ids differ.
fn build_coding_manifest(op_name: &str, variant: u8) -> HarnessManifest {
    let mut m = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "coding-harness-v0".into(),
        artifact_digest: CODING_ARTIFACT_PLACEHOLDER.into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: CODING_ENDPOINT.into(),
        operation_name: op_name.into(),
        description: format!("{op_name} variant {variant}"),
        input_schema: json!({
            "type": "object",
            "properties": {
                "workspace_id": {"type": "string", "enum": ["agent-dev"]},
                "marker": {"type": "string"}
            },
            "required": ["workspace_id", "marker"],
            "additionalProperties": false
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "ok": {"type": "boolean"},
                "variant": {"type": "integer"}
            },
            "required": ["ok", "variant"],
            "additionalProperties": false
        }),
        idempotent: true,
        created_at: chrono::Utc::now(),
    };
    // Encode variant into the description+schema so manifest_id differs per
    // variant even though the immutable fields are identical.
    m.description = format!("{op_name} variant {variant}");
    m.input_schema["properties"]["marker"] =
        json!({"type": "string", "enum": [format!("v{variant}")]});
    m.output_schema["properties"]["variant"] =
        json!({"type": "string", "enum": [format!("v{variant}")]});
    m.manifest_id = m.compute_manifest_id().expect("manifest_id");
    m
}

/// Build the proposal body for a manifest. The caller must have already stored
/// the artifact bytes in `store` (so the manifest's `artifact_digest` resolves).
fn proposal_body_for(
    store: &crate::capabilities::store::ContentStore,
    m: &HarnessManifest,
) -> Result<Value> {
    let manifest_bytes = serde_json::to_vec(m)?;
    let manifest_digest = store.store(&manifest_bytes)?;
    let evidence_digest = store.store(br#"{"attestation":"coding-schema-upgrade"}"#)?;
    Ok(json!({
        "target_agent_id": "main",
        "artifact_ref": "artifact.bin",
        "artifact_digest": m.artifact_digest,
        "manifest_ref": "manifest.json",
        "manifest_digest": manifest_digest.as_str(),
        "evidence_ref": "evidence.json",
        "evidence_digest": evidence_digest.as_str(),
        "requested_operations": [m.operation_name],
        "risk_summary": "coding schema upgrade",
    }))
}

/// Store the canonical coding-harness artifact bytes in `store` and return the
/// resulting digest. This is the single source of the artifact_digest used
/// across all seven manifests (both old and upgraded), so the schema-only
/// immutable-field check (artifact_digest unchanged) holds.
fn store_coding_artifact(store: &crate::capabilities::store::ContentStore) -> Result<String> {
    let bytes = b"#!/bin/sh\necho coding-harness\n";
    let digest = store.store(bytes)?;
    Ok(digest.as_str().to_string())
}

/// Activate all seven coding operations via the create path, returning the
/// initial snapshot and a map of op_name → old manifest.
fn activate_seven_ops(
    journal: &JournalStore,
) -> Result<(String, Vec<(String, HarnessManifest)>, String)> {
    let gw = gateway();
    let mut old_manifests = Vec::new();
    let mut real_artifact = String::new();
    for op in CODING_OPS {
        let dir = std::env::temp_dir().join(format!(
            "coding_create_{}_{}_{}",
            std::process::id(),
            op.replace('.', "_"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir)?;
        let store = crate::capabilities::store::ContentStore::new(dir.join("store"));
        let artifact_digest = store_coding_artifact(&store)?;
        real_artifact = artifact_digest.clone();
        let mut m = build_coding_manifest(op, 0);
        m.artifact_digest = artifact_digest;
        m.manifest_id = m.compute_manifest_id()?;

        let body = proposal_body_for(&store, &m)?;
        let resp = crate::server::capability_routes::handle_submit_proposal(
            journal,
            &gw,
            &body,
            "capability_submitter",
            &AgentId("main".to_string()),
        )?;
        let pid = resp.proposal_id;
        let dec = json!({
            "decision": "approved",
            "artifact_digest": m.artifact_digest,
            "manifest_digest": body["manifest_digest"].clone(),
        });
        handle_decision(
            journal,
            &gw,
            &store,
            &pid,
            &dec,
            "approval_workflow",
            &AgentId("main".to_string()),
        )?;
        old_manifests.push((op.to_string(), m));
    }
    let snap = journal.current_registry_snapshot_id()?;
    Ok((snap, old_manifests, real_artifact))
}

/// Build + submit + activate a schema-only upgrade for `op` from `old`.
/// Returns (new_manifest_id, new_snapshot_id, proposal_id, decision_id).
fn upgrade_one_op(
    journal: &JournalStore,
    old: &HarnessManifest,
) -> Result<(String, String, String, String)> {
    let gw = gateway();
    let dir = std::env::temp_dir().join(format!(
        "coding_up_{}_{}_{}",
        std::process::id(),
        old.operation_name.replace('.', "_"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir)?;
    let store = crate::capabilities::store::ContentStore::new(dir.join("store"));
    // The upgrade setup's own store must hold the SAME artifact bytes the old
    // manifest references; store_coding_artifact produces a deterministic
    // digest for the canonical bytes, so it equals old.artifact_digest.
    let artifact_digest = store_coding_artifact(&store)?;
    let mut new_manifest = build_coding_manifest(&old.operation_name, 1);
    new_manifest.artifact_digest = artifact_digest;
    new_manifest.manifest_id = new_manifest.compute_manifest_id()?;
    let body = proposal_body_for(&store, &new_manifest)?;
    let resp = crate::server::capability_routes::handle_submit_proposal(
        journal,
        &gw,
        &body,
        "capability_submitter",
        &AgentId("main".to_string()),
    )?;
    let pid = resp.proposal_id;
    let dec = json!({
        "decision": "approved",
        "artifact_digest": new_manifest.artifact_digest,
        "manifest_digest": body["manifest_digest"].clone(),
    });
    let result = handle_decision(
        journal,
        &gw,
        &store,
        &pid,
        &dec,
        "approval_workflow",
        &AgentId("main".to_string()),
    )?;
    let new_snap = result["activated_snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    Ok((
        new_manifest.manifest_id.clone(),
        new_snap,
        pid.clone(),
        format!("schema_upgrade:{pid}"),
    ))
}

#[test]
fn seven_coding_ops_sequential_schema_upgrade() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let gw = gateway();
    let _ = &gw;

    // 1. Initial snapshot with seven old manifests.
    let (s_initial, old_manifests, _real_artifact) = activate_seven_ops(&journal)?;
    assert_eq!(old_manifests.len(), 7);

    // 2. Sequentially upgrade each op. Each must produce a new snapshot and
    //    previously upgraded ops must NOT roll back.
    let mut new_ids: Vec<(String, String)> = Vec::new(); // (op, new_manifest_id)
    let mut last_snap = s_initial.clone();
    let mut proposal_ids: Vec<(String, String, String)> = Vec::new(); // (op, pid, decision_id)

    for (op, old) in &old_manifests {
        let (new_id, new_snap, pid, decision_id) = upgrade_one_op(&journal, old)?;
        assert_ne!(new_snap, last_snap, "snapshot must change on each upgrade");
        // Previously upgraded ops keep their NEW manifest in the latest snap.
        let snap = journal.load_registry_snapshot(&new_snap)?;
        for (prev_op, prev_new_id) in &new_ids {
            let spec = snap.lookup(prev_op).unwrap();
            assert_eq!(
                spec.binding_key, *prev_new_id,
                "{prev_op} rolled back during {op} upgrade"
            );
        }
        // The just-upgraded op points to its new manifest.
        assert_eq!(snap.lookup(op).unwrap().binding_key, new_id);
        new_ids.push((op.clone(), new_id.clone()));
        proposal_ids.push((op.clone(), pid, decision_id));
        last_snap = new_snap;
    }

    // 3. Final snapshot: all seven ops point to their NEW manifests.
    let final_snap = journal.load_registry_snapshot(&last_snap)?;
    for (op, new_id) in &new_ids {
        let spec = final_snap.lookup(op).unwrap();
        assert_eq!(
            spec.binding_key, *new_id,
            "{op} not on new manifest in final snap"
        );
    }

    // 4. All seven OLD manifests still queryable by id.
    for (_op, old) in &old_manifests {
        let m = journal
            .load_harness_manifest(&old.manifest_id)?
            .ok_or_else(|| anyhow!("old manifest {} disappeared", old.manifest_id))?;
        assert_eq!(m.manifest_id, old.manifest_id);
    }

    // 5. All seven NEW manifests exist.
    for (_op, new_id) in &new_ids {
        assert!(
            manifest_exists(&journal, new_id),
            "new manifest {new_id} missing"
        );
    }

    // 6. Each operation has exactly 2 manifest rows (old + new).
    for op in CODING_OPS {
        assert_eq!(
            manifest_count_for_operation(&journal, op),
            2,
            "{op} row count"
        );
    }

    // 7. Event payloads are exact: action, operation_name, old/new manifest_id,
    //    proposal_id, decision_id, old/new snapshot IDs.
    let payloads = schema_upgrade_payloads(&journal);
    assert_eq!(payloads.len(), 7, "exactly seven schema_upgrade events");
    for payload in &payloads {
        let op = payload["operation_name"].as_str().unwrap();
        assert_eq!(payload["action"], "schema_upgrade");
        let (exp_pid, exp_decision, _) = proposal_ids
            .iter()
            .find(|(p_op, _, _)| p_op == op)
            .map(|(_, pid, dec)| (pid.clone(), dec.clone(), String::new()))
            .unwrap();
        let exp_new_id = new_ids
            .iter()
            .find(|(p_op, _)| p_op == op)
            .map(|(_, id)| id.clone())
            .unwrap();
        let exp_old_id = old_manifests
            .iter()
            .find(|(p_op, _)| p_op == op)
            .map(|(_, m)| m.manifest_id.clone())
            .unwrap();
        assert_eq!(payload["proposal_id"], exp_pid, "{op} proposal_id");
        assert_eq!(payload["decision_id"], exp_decision, "{op} decision_id");
        assert_eq!(
            payload["old_manifest_id"], exp_old_id,
            "{op} old_manifest_id"
        );
        assert_eq!(
            payload["new_manifest_id"], exp_new_id,
            "{op} new_manifest_id"
        );
        assert!(
            payload
                .get("previous_snapshot_id")
                .and_then(|v| v.as_str())
                .is_some(),
            "{op} previous_snapshot_id present"
        );
        assert!(
            payload
                .get("new_snapshot_id")
                .and_then(|v| v.as_str())
                .is_some(),
            "{op} new_snapshot_id present"
        );
    }

    // 8. Final journal hash chain valid.
    assert!(journal.verify_hash_chain()?);
    Ok(())
}
