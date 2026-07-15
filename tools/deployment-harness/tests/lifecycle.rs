use agent_core_kernel::capabilities::store::ContentStore;
use agent_core_kernel::domain::{
    DeploymentIntent, ListenPolicy, RollbackPolicy, ServiceHealthcheck, ServiceManifest,
    TargetKind, UpgradePolicy, DEPLOYMENT_PROTOCOL, SERVICE_MANIFEST_SCHEMA,
};
use deployment_harness::config::DeploymentHarnessConfig;
use deployment_harness::manager;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn config(root: &TempDir) -> DeploymentHarnessConfig {
    let artifact_root = root.path().join("artifacts");
    let state_root = root.path().join("state");
    std::fs::create_dir_all(&artifact_root).unwrap();
    std::fs::create_dir_all(&state_root).unwrap();
    DeploymentHarnessConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        artifact_root,
        state_root,
        control_token: "c".repeat(32),
        event_observe_url: "http://127.0.0.1:4130/v1/events".into(),
        event_observe_token: "o".repeat(32),
    }
}

fn manifest(artifact_digest: String, version: &str) -> ServiceManifest {
    let mut manifest = ServiceManifest {
        schema_version: SERVICE_MANIFEST_SCHEMA.into(),
        manifest_id: String::new(),
        component_id: "fixture-service".into(),
        kind: TargetKind::HookConsumerService,
        artifact_digest,
        entrypoint: "artifact".into(),
        runtime_profile: "managed-service-v0".into(),
        version: version.into(),
        required_contracts: vec!["event.observe.v0".into()],
        requested_permissions: vec!["journal.observe".into()],
        listen_policy: ListenPolicy {
            host: "127.0.0.1".into(),
            port: 0,
            exposure: "loopback".into(),
        },
        healthcheck: ServiceHealthcheck {
            method: "GET".into(),
            path: "/health".into(),
            timeout_ms: 5_000,
        },
        state_path: "state".into(),
        upgrade_policy: UpgradePolicy {
            strategy: "replace_after_ready".into(),
            require_healthy_before_switch: true,
        },
        rollback_policy: RollbackPolicy {
            retain_previous_versions: 2,
            automatic_on_health_failure: true,
        },
    };
    manifest.manifest_id = manifest.compute_manifest_id().unwrap();
    manifest
}

fn intent(manifest_digest: String, manifest: &ServiceManifest, suffix: &str) -> DeploymentIntent {
    let mut intent = DeploymentIntent {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        invocation_id: format!("invocation_{suffix}"),
        intent_id: String::new(),
        proposal_id: format!("proposal_{suffix}"),
        decision_id: format!("decision_{suffix}"),
        service_manifest_digest: manifest_digest,
        artifact_digest: manifest.artifact_digest.clone(),
        expected_version: manifest.version.clone(),
        action: "install_start".into(),
    };
    intent.intent_id = intent.expected_intent_id();
    intent
}

fn store_release(
    store: &ContentStore,
    executable_path: &str,
    version: &str,
) -> (ServiceManifest, DeploymentIntent) {
    let executable = std::fs::read(executable_path).unwrap();
    let artifact_digest = store.store(&executable).unwrap().as_str().to_string();
    let manifest = manifest(artifact_digest, version);
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let manifest_digest = store.store(&manifest_bytes).unwrap().as_str().to_string();
    let intent = intent(manifest_digest, &manifest, version);
    (manifest, intent)
}

#[test]
fn deploy_replay_upgrade_rollback_and_disable() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    config.validate().unwrap();
    let store = ContentStore::new(config.artifact_root.clone());
    let (v1_manifest, v1_intent) = store_release(
        &store,
        env!("CARGO_BIN_EXE_deployment-fixture-service"),
        "0.1.0",
    );
    let (_, v2_intent) = store_release(
        &store,
        env!("CARGO_BIN_EXE_deployment-fixture-service-v2"),
        "0.2.0",
    );

    let first = manager::deploy(&config, &serde_json::to_vec(&v1_intent).unwrap()).unwrap();
    first
        .validate_for(&v1_intent, &v1_manifest.component_id)
        .unwrap();
    assert!(!first.replayed);
    assert_eq!(
        manager::status(&config, &v1_manifest.component_id).unwrap()["status"],
        "healthy"
    );

    let replay = manager::deploy(&config, &serde_json::to_vec(&v1_intent).unwrap()).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.deployment_id, first.deployment_id);

    let active_path = config
        .state_root
        .join("components/fixture-service/active.json");
    let active: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&active_path).unwrap()).unwrap();
    let pid = active["pid"].as_u64().unwrap() as i32;
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while manager::status(&config, "fixture-service").unwrap()["health_status"] == "ready" {
        assert!(Instant::now() < deadline, "fixture did not stop");
        std::thread::sleep(Duration::from_millis(25));
    }
    manager::reconcile(&config).unwrap();
    let recovered = manager::status(&config, "fixture-service").unwrap();
    assert_eq!(recovered["health_status"], "ready");
    assert_eq!(recovered["endpoint"], first.endpoint);

    let upgraded = manager::deploy(&config, &serde_json::to_vec(&v2_intent).unwrap()).unwrap();
    assert_eq!(upgraded.version, "0.2.0");
    assert_eq!(
        upgraded.previous_artifact_digest.as_deref(),
        Some(v1_manifest.artifact_digest.as_str())
    );
    let downgrade = manager::deploy(&config, &serde_json::to_vec(&v1_intent).unwrap())
        .unwrap_err()
        .to_string();
    assert!(downgrade.contains("NOT_MONOTONIC"), "{downgrade}");

    let rollback = manager::rollback(&config, "fixture-service", "decision_rollback").unwrap();
    assert_eq!(rollback.status, "rolled_back");
    assert_eq!(rollback.version, "0.1.0");
    let rollback_replay =
        manager::rollback(&config, "fixture-service", "decision_rollback").unwrap();
    assert_eq!(rollback_replay, rollback);

    let disabled = manager::disable(&config, "fixture-service", "decision_disable").unwrap();
    assert_eq!(disabled.status, "disabled");
    let disable_replay = manager::disable(&config, "fixture-service", "decision_disable").unwrap();
    assert_eq!(disable_replay, disabled);
    assert_eq!(
        manager::status(&config, "fixture-service").unwrap()["status"],
        "disabled"
    );
}
