//! Capability Host integration tests.
mod common;
use common::{deploy_calculator, send_http, start_capability_host, store_artifact};
use serde_json::json;
use std::io::{Read, Write};

#[test]
fn valid_artifact_returns_result() {
    let root = tmpdir("ch_valid");
    let calc = fixture_path!("calculator");
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (deploy_code, deployed) = deploy_calculator(
        &root,
        port,
        &digest,
        "proposal-1",
        "decision-1",
        "snapshot-1",
    );
    assert_eq!(deploy_code, 200, "{deployed}");
    let manifest_id = deployed["manifest_id"].as_str().unwrap();
    let request = json!({
        "protocol_version":"external-harness-v1","invocation_id":"t1","operation":"external.calculator",
        "arguments":{"operation":"multiply","a":6,"b":7},"manifest_id":manifest_id,
        "artifact_digest":digest,"registry_snapshot_id":"snapshot-1",
    })
    .to_string();
    let (code, body) = send_http("127.0.0.1", port, &request);
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"], 42);
    assert!(r["capability_host_execution_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("che_"));
    let (_, replay_body) = send_http("127.0.0.1", port, &request);
    let replay: serde_json::Value = serde_json::from_str(&replay_body).unwrap();
    assert_eq!(
        r["capability_host_execution_id"],
        replay["capability_host_execution_id"]
    );

    // SAME_CONTENT_NEW_SNAPSHOT_ACCEPTED:
    // Deployment binding is authorized by content-addressed identity
    // (manifest_id, artifact_digest, operation_name), not by registry snapshot.
    // A different registry_snapshot_id must be accepted when content is unchanged.
    let mut different_snapshot: serde_json::Value = serde_json::from_str(&request).unwrap();
    different_snapshot["registry_snapshot_id"] = json!("snapshot-replaced");
    let (code2, accepted) = send_http("127.0.0.1", port, &different_snapshot.to_string());
    assert_eq!(code2, 200);
    let accepted: serde_json::Value = serde_json::from_str(&accepted).unwrap();
    assert_eq!(accepted["ok"], true, "different snapshot must be accepted: {accepted}");
}

#[test]
fn artifact_digest_mismatch_is_rejected() {
    let root = tmpdir("ch_mismatch");
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (code, body) = send_http("127.0.0.1", port, &json!({
        "protocol_version":"external-harness-v1","invocation_id":"t2","operation":"external.calculator",
        "arguments":{"operation":"multiply","a":6,"b":7},"manifest_id":"m",
        "artifact_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "registry_snapshot_id":"snapshot-1",
    }).to_string());
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["error_code"], "capability_not_deployed");
}

#[test]
fn unsupported_protocol_is_rejected() {
    let root = tmpdir("ch_proto");
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (code, body) = send_http("127.0.0.1", port, &json!({
        "protocol_version":"external-harness-v2","invocation_id":"t3","operation":"test.op",
        "arguments":{},"manifest_id":"m",
        "artifact_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "registry_snapshot_id":"snapshot-1",
    }).to_string());
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["ok"], false);
}

#[test]
fn missing_artifact_digest_is_rejected() {
    let root = tmpdir("ch_missing");
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (code, body) = send_http(
        "127.0.0.1",
        port,
        &json!({
            "protocol_version":"external-harness-v1","invocation_id":"t4","operation":"test.op",
            "arguments":{},"registry_snapshot_id":"snapshot-1",
        })
        .to_string(),
    );
    assert_eq!(code, 200);
    assert!(!body.contains(r#""ok":true"#));
}

#[test]
fn health_check_returns_ok() {
    let root = tmpdir("ch_health");
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    s.write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).unwrap();
    assert!(r.contains("200"));
    assert!(r.contains("ok"));
}

// ── SECURITY: changed manifest → rejected ──
#[test]
fn changed_manifest_id_is_rejected() {
    let root = tmpdir("ch_changed_manifest");
    let calc = fixture_path!("calculator");
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (deploy_code, deployed) = deploy_calculator(
        &root, port, &digest, "proposal-m1", "decision-m1", "snapshot-1",
    );
    assert_eq!(deploy_code, 200, "{deployed}");

    // Execute with a DIFFERENT manifest_id than what was deployed.
    let request = json!({
        "protocol_version":"external-harness-v1","invocation_id":"t-m1","operation":"external.calculator",
        "arguments":{"operation":"multiply","a":2,"b":3},"manifest_id":"fake-manifest-id-wrong",
        "artifact_digest":digest,"registry_snapshot_id":"snapshot-1",
    }).to_string();
    let (code, body) = send_http("127.0.0.1", port, &request);
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        r["error_code"], "deployment_binding_mismatch",
        "wrong manifest_id must be rejected: {r}"
    );
}

// ── SECURITY: changed artifact digest → rejected ──
#[test]
fn changed_artifact_digest_on_deployed_operation_rejected() {
    let root = tmpdir("ch_changed_artifact");
    let calc = fixture_path!("calculator");
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (deploy_code, deployed) = deploy_calculator(
        &root, port, &digest, "proposal-a1", "decision-a1", "snapshot-1",
    );
    assert_eq!(deploy_code, 200, "{deployed}");
    let manifest_id = deployed["manifest_id"].as_str().unwrap();

    // Execute with a DIFFERENT artifact_digest (wrong hash) than deployed.
    let request = json!({
        "protocol_version":"external-harness-v1","invocation_id":"t-a1","operation":"external.calculator",
        "arguments":{"operation":"multiply","a":2,"b":3},"manifest_id":manifest_id,
        "artifact_digest":"sha256:00001111222233334444555566667777888899990000aaaaabbbbbcccccdddddeeee",
        "registry_snapshot_id":"snapshot-1",
    }).to_string();
    let (code, body) = send_http("127.0.0.1", port, &request);
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        r["error_code"], "deployment_binding_mismatch",
        "wrong artifact_digest must be rejected: {r}"
    );
}

// ── SECURITY: wrong operation name → rejected ──
#[test]
fn changed_operation_name_rejected() {
    let root = tmpdir("ch_changed_op");
    let calc = fixture_path!("calculator");
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (deploy_code, deployed) = deploy_calculator(
        &root, port, &digest, "proposal-op1", "decision-op1", "snapshot-1",
    );
    assert_eq!(deploy_code, 200, "{deployed}");
    let manifest_id = deployed["manifest_id"].as_str().unwrap();

    // Execute with a DIFFERENT operation_name than deployed.
    let request = json!({
        "protocol_version":"external-harness-v1","invocation_id":"t-op1","operation":"external.unknown",
        "arguments":{"operation":"multiply","a":2,"b":3},"manifest_id":manifest_id,
        "artifact_digest":digest,"registry_snapshot_id":"snapshot-1",
    }).to_string();
    let (code, body) = send_http("127.0.0.1", port, &request);
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        r["error_code"], "capability_not_deployed",
        "wrong operation must be rejected: {r}"
    );
}

fn tmpdir(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("ch_{label}_{}_{nonce}", std::process::id()));
    std::fs::create_dir_all(&d).ok();
    d
}
