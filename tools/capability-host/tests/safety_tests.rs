//! Capability Host safety/edge-case tests.
mod common;
use common::*;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn non_json_stdout_is_rejected() {
    let root = tmpdir("ch_nonjson");
    let digest = create_script_artifact(&root, "#!/bin/sh\necho hello\n");
    let (port, _) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));
    let (code, body) = send_http(
        "127.0.0.1",
        port,
        &serde_json::json!({
            "protocol_version":"external-harness-v1","invocation_id":"nj1","operation":"t",
            "arguments":{},"manifest_id":"m","artifact_digest":digest,
        })
        .to_string(),
    );
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["error_code"], "artifact_protocol_error");
}

#[test]
fn nonzero_exit_is_structured_failure() {
    let root = tmpdir("ch_badxit");
    let digest = create_script_artifact(&root, "#!/bin/sh\necho '{\"ok\":true}'\nexit 1\n");
    let (port, _) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));
    let (code, body) = send_http(
        "127.0.0.1",
        port,
        &serde_json::json!({
            "protocol_version":"external-harness-v1","invocation_id":"nz1","operation":"t",
            "arguments":{},"manifest_id":"m","artifact_digest":digest,
        })
        .to_string(),
    );
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["error_code"], "artifact_failed");
}

#[test]
fn artifact_timeout_kills_process_tree() {
    let root = tmpdir("ch_timeout");
    let digest = create_script_artifact(&root, "#!/bin/sh\nsleep 60 &\nsleep 120\necho done\n");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = Arc::new(AtomicBool::new(false));
    let s = shutdown.clone();
    let rc = root.clone();
    thread::spawn(move || {
        let cfg = capability_host::config::CapabilityHostConfig {
            listen_addr: format!("127.0.0.1:{port}"),
            artifact_root: rc,
            exec_timeout: Duration::from_secs(2),
            max_stdout_bytes: 65536,
            max_stderr_bytes: 65536,
        };
        for stream in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(mut stream) = stream {
                let response = handle_request(&mut stream, &cfg);
                let _ = stream.write_all(response.as_bytes());
            }
        }
    });
    thread::sleep(Duration::from_millis(200));
    let (code, body) = send_http(
        "127.0.0.1",
        port,
        &serde_json::json!({
            "protocol_version":"external-harness-v1","invocation_id":"to1","operation":"t",
            "arguments":{},"manifest_id":"m","artifact_digest":digest,
        })
        .to_string(),
    );
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["error_code"], "artifact_timeout");
}

#[test]
fn large_stdout_does_not_deadlock() {
    let root = tmpdir("ch_largeout");
    let digest = create_script_artifact(&root, "#!/bin/sh\ni=0;while [ $i -lt 5000 ];do echo \"line $i xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\";i=$((i+1));done\n");
    let (port, _) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));
    let (code, body) = send_http(
        "127.0.0.1",
        port,
        &serde_json::json!({
            "protocol_version":"external-harness-v1","invocation_id":"lo1","operation":"t",
            "arguments":{},"manifest_id":"m","artifact_digest":digest,
        })
        .to_string(),
    );
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(r["ok"], false, "large stdout should fail");
}

#[test]
fn request_cannot_supply_artifact_path() {
    let root = tmpdir("ch_pathinj");
    let calc = match calculator_binary() {
        Some(b) => b,
        None => {
            eprintln!("skip");
            return;
        }
    };
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));
    let (code, body) = send_http("127.0.0.1", port, &serde_json::json!({
        "protocol_version":"external-harness-v1","invocation_id":"pi1","operation":"external.calculator",
        "arguments":{"operation":"multiply","a":6,"b":7,"artifact_path":"/etc/passwd","entrypoint":"../../evil"},
        "manifest_id":"m","artifact_digest":digest,
    }).to_string());
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"], 42);
}

fn tmpdir(label: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ch_safety_{label}_{}", std::process::id()));
    std::fs::create_dir_all(&d).ok();
    d
}
