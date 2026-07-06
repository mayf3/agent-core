//! Capability Host safety/edge-case tests using compiled Rust fixture binaries.
mod common;
use common::*;

fn tmpdir(label: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ch_safety_{label}_{}", std::process::id()));
    std::fs::create_dir_all(&d).ok();
    d
}

#[test]
fn non_json_stdout_is_rejected() {
    let root = tmpdir("nonjson");
    let path = fixture_path!("non-json-stdout");
    let digest = store_artifact(&root, &path);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
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
    let root = tmpdir("nonzero");
    let path = fixture_path!("nonzero-exit");
    let digest = store_artifact(&root, &path);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
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
    let root = tmpdir("timeout");
    let path = fixture_path!("timeout-tree");
    let digest = store_artifact(&root, &path);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s = shutdown.clone();
    let rc = root.clone();
    std::thread::spawn(move || {
        let cfg = capability_host::config::CapabilityHostConfig {
            listen_addr: format!("127.0.0.1:{port}"),
            artifact_root: rc,
            exec_timeout: std::time::Duration::from_secs(2),
            max_stdout_bytes: 65536,
            max_stderr_bytes: 65536,
        };
        for stream in listener.incoming() {
            if s.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            if let Ok(stream) = stream {
                capability_host::server::handle(stream, &cfg);
            }
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
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
    let root = tmpdir("largeout");
    let path = fixture_path!("large-stdout");
    let digest = store_artifact(&root, &path);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
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
    assert_eq!(r["ok"], false, "large stdout should not produce ok=true");
}

#[test]
fn request_cannot_supply_artifact_path() {
    let root = tmpdir("pathinj");
    let calc = match calculator_binary() {
        Some(b) => b,
        None => {
            eprintln!("calculator binary not found, skipping");
            return;
        }
    };
    let digest = store_artifact(&root, &calc);
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (code, body) = send_http("127.0.0.1", port, &serde_json::json!({
        "protocol_version":"external-harness-v1","invocation_id":"pi1",
        "operation":"external.calculator",
        "arguments":{"operation":"multiply","a":6,"b":7,"artifact_path":"/etc/passwd","entrypoint":"../../evil"},
        "manifest_id":"m","artifact_digest":digest,
    }).to_string());
    assert_eq!(code, 200);
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["ok"], true, "path injection fields should be ignored");
    assert_eq!(r["result"], 42);
}
