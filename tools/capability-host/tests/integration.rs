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

    let mut wrong_snapshot: serde_json::Value = serde_json::from_str(&request).unwrap();
    wrong_snapshot["registry_snapshot_id"] = json!("snapshot-replaced");
    let (_, rejected) = send_http("127.0.0.1", port, &wrong_snapshot.to_string());
    let rejected: serde_json::Value = serde_json::from_str(&rejected).unwrap();
    assert_eq!(rejected["error_code"], "deployment_binding_mismatch");
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
