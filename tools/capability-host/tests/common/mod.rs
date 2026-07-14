//! Shared helpers for Capability Host integration tests.
#![allow(dead_code)]
//! The `handle_request` / `execute_artifact` test helpers have been removed.
//! Tests now go through the production `capability_host::server::serve`.
//! Use `start_capability_host` to boot a real server on a random port.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub const CONTROL_TOKEN: &str = "test-capability-control-token-00000001";
pub const EXECUTION_TOKEN: &str = "test-capability-execution-token-00000002";

/// Path to a fixture binary by providing its hyphenated name as a string.
#[macro_export]
macro_rules! fixture_path {
    ($name:expr) => {{
        let bin_name = concat!("fixture-", $name);
        let env_var = concat!("CARGO_BIN_EXE_fixture_", $name);
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        let p = match ::std::env::var(env_var) {
            Ok(p) => ::std::path::PathBuf::from(p),
            Err(_) => ::std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(profile)
                .join(bin_name),
        };
        assert!(p.exists(), "fixture {} not found at {:?}", $name, p);
        p
    }};
}

/// Start Capability Host on a random port using the production server.
pub fn start_capability_host(artifact_root: &PathBuf) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = Arc::new(AtomicBool::new(false));
    let s = shutdown.clone();
    let root = artifact_root.clone();
    thread::spawn(move || {
        let config = Arc::new(capability_host::config::CapabilityHostConfig {
            listen_addr: format!("127.0.0.1:{port}"),
            artifact_root: root,
            exec_timeout: Duration::from_secs(30),
            max_stdout_bytes: 65536,
            max_stderr_bytes: 65536,
            control_token: CONTROL_TOKEN.into(),
            execution_token: EXECUTION_TOKEN.into(),
        });
        // Use the production server's handle function
        for stream in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(stream) = stream {
                let config = Arc::clone(&config);
                thread::spawn(move || capability_host::server::handle(stream, &config));
            }
        }
    });
    (port, shutdown)
}

pub fn send_http(host: &str, port: u16, body: &str) -> (u16, String) {
    send_http_path(host, port, "/execute", EXECUTION_TOKEN, body)
}

pub fn send_http_path(host: &str, port: u16, path: &str, token: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(format!("{host}:{port}")).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let request = format!("POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let code: u16 = response
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let json_body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (code, json_body)
}

pub fn deploy_calculator(
    artifact_root: &PathBuf,
    port: u16,
    artifact_digest: &str,
    proposal_id: &str,
    decision_id: &str,
    snapshot_id: &str,
) -> (u16, serde_json::Value) {
    let body = calculator_deploy_body(
        artifact_root,
        port,
        artifact_digest,
        proposal_id,
        decision_id,
        snapshot_id,
    );
    let (code, response) = send_http_path("127.0.0.1", port, "/deploy", CONTROL_TOKEN, &body);
    (code, serde_json::from_str(&response).unwrap_or_default())
}

pub fn calculator_deploy_body(
    artifact_root: &PathBuf,
    port: u16,
    artifact_digest: &str,
    proposal_id: &str,
    decision_id: &str,
    snapshot_id: &str,
) -> String {
    use agent_core_kernel::capabilities::store::ContentStore;
    use agent_core_kernel::harness::manifest::HarnessManifest;
    use chrono::Utc;
    use serde_json::json;
    let mut manifest = HarnessManifest {
        manifest_id: String::new(),
        harness_id: "capability-host-v0".into(),
        artifact_digest: artifact_digest.into(),
        protocol_version: "external-harness-v1".into(),
        endpoint: format!("http://127.0.0.1:{port}/execute"),
        operation_name: "external.calculator".into(),
        description: "Approved calculator supporting add, subtract, multiply, and divide.".into(),
        input_schema: json!({
            "type":"object","properties":{
                "operation":{"type":"string","enum":["add","subtract","multiply","divide"]},
                "a":{"type":"number"},"b":{"type":"number"}
            },"required":["operation","a","b"],"additionalProperties":false
        }),
        output_schema: json!({"type":"number"}),
        idempotent: true,
        created_at: Utc::now(),
    };
    manifest.manifest_id = manifest.compute_manifest_id().unwrap();
    let manifest_digest = ContentStore::new(artifact_root.clone())
        .store(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    json!({
        "protocol_version":"capability-deploy-v1",
        "proposal_id":proposal_id,
        "decision_id":decision_id,
        "manifest_digest":manifest_digest.as_str(),
        "artifact_digest":artifact_digest,
        "target_registry_snapshot_id":snapshot_id,
    })
    .to_string()
}

pub fn store_artifact(artifact_root: &PathBuf, binary: &PathBuf) -> String {
    use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
    let bytes = std::fs::read(binary).unwrap();
    let digest = Sha256Digest::compute(&bytes);
    ContentStore::new(artifact_root.clone())
        .store(&bytes)
        .unwrap();
    digest.as_str().to_string()
}
