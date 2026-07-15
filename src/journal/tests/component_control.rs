use super::*;
use crate::domain::{
    ComponentControlIntent, ComponentControlReceipt, ComponentStatus, DeploymentReceipt,
    RegisteredComponent, TargetKind, DEPLOYMENT_PROTOCOL,
};
use rusqlite::params;

const V1_ARTIFACT: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const V2_ARTIFACT: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

fn component(version: &str, artifact: &str, deployment: &str) -> RegisteredComponent {
    RegisteredComponent {
        component_id: "dashboard".into(),
        kind: TargetKind::HookConsumerService,
        manifest_id: format!("manifest_{version}"),
        manifest_digest: format!(
            "sha256:{}",
            if version == "0.1.0" { "a" } else { "b" }.repeat(64)
        ),
        artifact_digest: artifact.into(),
        version: version.into(),
        endpoint: "http://127.0.0.1:7401".into(),
        deployment_id: deployment.into(),
        deployment_receipt_id: format!("receipt_{version}"),
        status: ComponentStatus::Healthy,
        required_contracts: vec!["event.observe.v0".into()],
        requested_permissions: vec!["journal.observe".into()],
    }
}

fn seed_versions(journal: &super::super::JournalStore) -> anyhow::Result<(String, String)> {
    let mut conn = journal.conn.lock().map_err(|_| anyhow::anyhow!("mutex"))?;
    let tx = conn.transaction()?;
    let v1 = component("0.1.0", V1_ARTIFACT, "deployment_v1");
    let v1_snapshot = super::super::component_registry::persist_snapshot(&tx, &[v1])?;
    tx.execute(
        "UPDATE component_registry_state SET active_snapshot_id=?1,version=version+1",
        params![v1_snapshot],
    )?;
    let v2 = component("0.2.0", V2_ARTIFACT, "deployment_v2");
    let mut v2_receipt = DeploymentReceipt {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        receipt_id: "receipt_0.2.0".into(),
        invocation_id: "invocation_v2".into(),
        intent_id: "intent_v2".into(),
        proposal_id: "proposal_v2".into(),
        decision_id: "decision_v2".into(),
        deployment_id: "deployment_v2".into(),
        component_id: "dashboard".into(),
        service_manifest_digest: format!("sha256:{}", "b".repeat(64)),
        artifact_digest: V2_ARTIFACT.into(),
        version: "0.2.0".into(),
        status: "healthy".into(),
        endpoint: "http://127.0.0.1:7401".into(),
        health_status: "ready".into(),
        log_ref: "components/dashboard/logs/0.2.0.log".into(),
        previous_artifact_digest: Some(V1_ARTIFACT.into()),
        started_at: "2026-07-15T00:00:00Z".into(),
        finished_at: "2026-07-15T00:00:01Z".into(),
        replayed: false,
    };
    v2_receipt.receipt_id = "receipt_0.2.0".into();
    tx.execute(
        "INSERT INTO component_deployment_receipts
         (receipt_id,deployment_id,invocation_id,proposal_id,decision_id,component_id,
          manifest_digest,artifact_digest,version,endpoint,health_status,log_ref,
          payload_json,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            v2_receipt.receipt_id,
            v2_receipt.deployment_id,
            v2_receipt.invocation_id,
            v2_receipt.proposal_id,
            v2_receipt.decision_id,
            v2_receipt.component_id,
            v2_receipt.service_manifest_digest,
            v2_receipt.artifact_digest,
            v2_receipt.version,
            v2_receipt.endpoint,
            v2_receipt.health_status,
            v2_receipt.log_ref,
            serde_json::to_string(&v2_receipt)?,
            "2026-07-15T00:00:01Z",
        ],
    )?;
    let v2_snapshot = super::super::component_registry::persist_snapshot(&tx, &[v2])?;
    tx.execute(
        "UPDATE component_registry_state SET active_snapshot_id=?1,version=version+1",
        params![v2_snapshot],
    )?;
    tx.commit()?;
    Ok((v1_snapshot, v2_snapshot))
}

fn intent(action: &str, snapshot: &str, deployment: &str, nonce: char) -> ComponentControlIntent {
    let mut intent = ComponentControlIntent {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        decision_id: String::new(),
        decision_nonce: nonce.to_string().repeat(32),
        principal_id: "feishu:open_id:owner".into(),
        component_id: "dashboard".into(),
        action: action.into(),
        expected_component_snapshot_id: snapshot.into(),
        expected_deployment_id: deployment.into(),
    };
    intent.decision_id = intent.expected_decision_id();
    intent
}

fn receipt(
    intent: &ComponentControlIntent,
    artifact: &str,
    version: &str,
    deployment: &str,
    status: &str,
    health: &str,
) -> ComponentControlReceipt {
    let mut receipt = ComponentControlReceipt {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        ok: true,
        receipt_id: String::new(),
        action: intent.action.clone(),
        decision_id: intent.decision_id.clone(),
        component_id: intent.component_id.clone(),
        deployment_id: deployment.into(),
        artifact_digest: artifact.into(),
        version: version.into(),
        status: status.into(),
        endpoint: "http://127.0.0.1:7501".into(),
        health_status: health.into(),
        log_ref: format!("components/dashboard/logs/{version}.log"),
    };
    receipt.receipt_id = receipt.expected_receipt_id();
    receipt
}

#[test]
fn governed_rollback_and_disable_publish_replay_safe_snapshots() -> anyhow::Result<()> {
    let journal = super::super::JournalStore::in_memory()?;
    let (_, v2_snapshot) = seed_versions(&journal)?;

    let rollback_intent = intent("rollback", &v2_snapshot, "deployment_v2", 'r');
    journal.record_component_control_intent(&rollback_intent, "owner")?;
    let overlapping = intent("disable", &v2_snapshot, "deployment_v2", 'o');
    let overlap_error = journal
        .record_component_control_intent(&overlapping, "owner")
        .unwrap_err()
        .to_string();
    assert!(overlap_error.contains("EFFECT_IN_FLIGHT"));
    let rollback_receipt = receipt(
        &rollback_intent,
        V1_ARTIFACT,
        "0.1.0",
        "deployment_v1",
        "rolled_back",
        "ready",
    );
    let rolled_back =
        journal.settle_component_control_atomic(&rollback_intent, &rollback_receipt)?;
    assert_eq!(rolled_back.component.status, ComponentStatus::RolledBack);
    assert_eq!(rolled_back.component.artifact_digest, V1_ARTIFACT);
    let durable_replay = journal
        .replay_component_control(&rollback_intent)?
        .expect("settled control decision");
    assert!(durable_replay.replayed);
    assert_eq!(durable_replay.receipt_id, rollback_receipt.receipt_id);
    let replay = journal.settle_component_control_atomic(&rollback_intent, &rollback_receipt)?;
    assert!(replay.replayed);
    assert_eq!(replay.target_snapshot_id, rolled_back.target_snapshot_id);

    let disable_intent = intent(
        "disable",
        &rolled_back.target_snapshot_id,
        "deployment_v1",
        'd',
    );
    journal.record_component_control_intent(&disable_intent, "owner")?;
    let disable_receipt = receipt(
        &disable_intent,
        V1_ARTIFACT,
        "0.1.0",
        "deployment_v1",
        "disabled",
        "unavailable",
    );
    let disabled = journal.settle_component_control_atomic(&disable_intent, &disable_receipt)?;
    assert_eq!(disabled.component.status, ComponentStatus::Disabled);

    let events = journal.events()?;
    for kind in [
        JournalEventKind::ComponentControlIntentRecorded,
        JournalEventKind::ComponentControlReceiptRecorded,
        JournalEventKind::ComponentRolledBack,
        JournalEventKind::ComponentDisabled,
    ] {
        assert!(events.iter().any(|event| event.kind == kind), "{kind:?}");
    }
    Ok(())
}

#[test]
fn control_rejects_wrong_owner_before_external_effect() -> anyhow::Result<()> {
    let journal = super::super::JournalStore::in_memory()?;
    let (_, v2_snapshot) = seed_versions(&journal)?;
    let rollback = intent("rollback", &v2_snapshot, "deployment_v2", 'x');
    let error = journal
        .record_component_control_intent(&rollback, "different-owner")
        .unwrap_err()
        .to_string();
    assert!(error.contains("OWNER_MISMATCH"));
    Ok(())
}
