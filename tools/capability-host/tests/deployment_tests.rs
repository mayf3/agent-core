mod common;
use common::*;

fn root(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "capability_host_deploy_{label}_{}_{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn tokens_are_endpoint_specific_and_fail_closed() {
    let root = root("auth");
    let calc = fixture_path!("calculator");
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let body = calculator_deploy_body(
        &root,
        port,
        &digest,
        "proposal-auth",
        "decision-auth",
        "snapshot-auth",
    );

    let (code, _) = send_http_path("127.0.0.1", port, "/deploy", EXECUTION_TOKEN, &body);
    assert_eq!(code, 401);
    let (code, _) = send_http_path("127.0.0.1", port, "/execute", CONTROL_TOKEN, "{}");
    assert_eq!(code, 401);
    let (code, _) = send_http_path("127.0.0.1", port, "/deploy", "", &body);
    assert_eq!(code, 401);
}

#[test]
fn twenty_concurrent_identical_deploys_create_one_record() {
    let root = root("concurrent");
    let calc = fixture_path!("calculator");
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let body = calculator_deploy_body(
        &root,
        port,
        &digest,
        "proposal-concurrent",
        "decision-concurrent",
        "snapshot-concurrent",
    );

    let mut threads = Vec::new();
    for _ in 0..20 {
        let body = body.clone();
        threads.push(std::thread::spawn(move || {
            send_http_path("127.0.0.1", port, "/deploy", CONTROL_TOKEN, &body)
        }));
    }
    let mut deployment_ids = std::collections::BTreeSet::new();
    let mut fresh = 0;
    for thread in threads {
        let (code, response) = thread.join().unwrap();
        assert_eq!(code, 200, "{response}");
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["proposal_id"], "proposal-concurrent");
        assert_eq!(value["decision_id"], "decision-concurrent");
        assert!(value["manifest_digest"]
            .as_str()
            .unwrap_or("")
            .starts_with("sha256:"));
        deployment_ids.insert(value["deployment_id"].as_str().unwrap().to_string());
        if value["replayed"] == false {
            fresh += 1;
        }
    }
    assert_eq!(deployment_ids.len(), 1);
    assert_eq!(fresh, 1);
    let record = root
        .join(".capability-host")
        .join("external.calculator.json");
    assert!(record.is_file());
}

#[test]
fn conflicting_deploy_cannot_replace_allowlist() {
    let root = root("conflict");
    let calc = fixture_path!("calculator");
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let (code, first) = deploy_calculator(
        &root,
        port,
        &digest,
        "proposal-first",
        "decision-first",
        "snapshot-first",
    );
    assert_eq!(code, 200, "{first}");
    let (code, conflict) = deploy_calculator(
        &root,
        port,
        &digest,
        "proposal-second",
        "decision-second",
        "snapshot-second",
    );
    assert_eq!(code, 409);
    assert_eq!(conflict["error_code"], "deployment_conflict");

    let request = serde_json::json!({
        "protocol_version":"external-harness-v1",
        "invocation_id":"after-conflict",
        "operation":"external.calculator",
        "arguments":{"operation":"multiply","a":6,"b":7},
        "manifest_id":first["manifest_id"],
        "artifact_digest":digest,
        "registry_snapshot_id":"snapshot-first",
    });
    let (code, response) = send_http("127.0.0.1", port, &request.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["result"], 42);
}

#[test]
fn lost_prepare_response_is_recovered_from_durable_record_after_restart() {
    let root = root("lost-response");
    let calculator = fixture_path!("calculator");
    let digest = store_artifact(&root, &calculator);
    let port = 43123;
    let body = calculator_deploy_body(
        &root,
        port,
        &digest,
        "proposal-lost-response",
        "decision-lost-response",
        "snapshot-lost-response",
    );
    let config = || capability_host::config::CapabilityHostConfig {
        listen_addr: format!("127.0.0.1:{port}"),
        artifact_root: root.clone(),
        exec_timeout: std::time::Duration::from_secs(3),
        max_stdout_bytes: 65536,
        max_stderr_bytes: 65536,
        control_token: CONTROL_TOKEN.into(),
        execution_token: EXECUTION_TOKEN.into(),
    };

    let first = capability_host::deployment::prepare(&config(), &body).unwrap();
    assert_eq!(first["replayed"], false);
    // Simulate the HTTP response being lost and the Host process restarting:
    // construct fresh process configuration and recover only from disk.
    let recovered = capability_host::deployment::prepare(&config(), &body).unwrap();
    assert_eq!(recovered["replayed"], true);
    for field in [
        "deployment_id",
        "proposal_id",
        "decision_id",
        "manifest_digest",
        "manifest_id",
        "artifact_digest",
        "target_registry_snapshot_id",
        "probe_execution_id",
    ] {
        assert_eq!(first[field], recovered[field], "field {field}");
    }
}
