//! Capability Host integration tests.
mod common;
use common::{calculator_binary, send_http, start_capability_host, store_artifact};
use serde_json::json;
use std::io::{Read, Write};

#[test]
fn valid_artifact_returns_result() {
    let root = tmpdir("ch_valid");
    let calc = match calculator_binary() {
        Some(b) => b,
        None => {
            eprintln!("skip");
            return;
        }
    };
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (code, body) = send_http("127.0.0.1", port, &json!({
        "protocol_version":"external-harness-v1","invocation_id":"t1","operation":"external.calculator",
        "arguments":{"operation":"multiply","a":6,"b":7},"manifest_id":"m","artifact_digest":digest,
    }).to_string());
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"], 42);
}

fn tmpdir(label: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ch_{label}_{}", std::process::id()));
    std::fs::create_dir_all(&d).ok();
    d
}

#[test]
fn artifact_digest_mismatch_is_rejected() {
    let root = tmpdir("ch_mismatch");
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (code, body) = send_http("127.0.0.1", port, &json!({
        "protocol_version":"external-harness-v1","invocation_id":"t2","operation":"test.op",
        "arguments":{},"manifest_id":"m",
        "artifact_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",
    }).to_string());
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["error_code"], "artifact_not_found");
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
    let (code, body) = send_http("127.0.0.1", port, &json!({
        "protocol_version":"external-harness-v1","invocation_id":"t4","operation":"test.op","arguments":{},
    }).to_string());
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
