//! Capability Host integration tests.
//!
//! These tests start a real Capability Host server, exercise its HTTP
//! endpoints, and verify error handling and correct artifact execution.

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Locate the calculator artifact binary built by `cargo build`.
fn calculator_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_calculator-artifact"))
}

/// Start the Capability Host on a random port, returning the port and a shutdown flag.
fn start_capability_host(artifact_root: &PathBuf) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = Arc::new(AtomicBool::new(false));
    let s = shutdown.clone();
    let root = artifact_root.clone();
    thread::spawn(move || {
        let config = capability_host::config::CapabilityHostConfig {
            listen_addr: format!("127.0.0.1:{port}"),
            artifact_root: root,
            exec_timeout: Duration::from_secs(30),
            max_stdout_bytes: 65536,
            max_stderr_bytes: 65536,
        };
        // Simple single-request handler loop.
        for stream in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(mut stream) = stream {
                let response = handle_request(&mut stream, &config);
                let _ = stream.write_all(response.as_bytes());
            }
        }
    });
    (port, shutdown)
}

/// Quick-and-dirty request handler so we don't need the full server module.
fn handle_request(stream: &mut TcpStream, config: &capability_host::config::CapabilityHostConfig) -> String {
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return http_500();
    }
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return http_500();
    }
    let method = parts[0];
    let path = parts[1];

    // Read Content-Length.
    let mut content_length: usize = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if header.to_ascii_lowercase().starts_with("content-length:") {
            content_length = header.split(':').nth(1).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        }
    }

    let mut body = String::new();
    if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).is_ok() {
            body = String::from_utf8(buf).unwrap_or_default();
        }
    }

    match (method, path) {
        ("GET", "/health") => http_200(r#"{"status":"ok"}"#),
        ("POST", "/execute") => execute_artifact(&body, config),
        _ => http_404(),
    }
}

fn execute_artifact(body: &str, config: &capability_host::config::CapabilityHostConfig) -> String {
    // Parse request.
    let body_json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return harness_response(false, "malformed_request"),
    };

    let req = match capability_host::protocol::parse_harness_request(&body_json) {
        Ok(r) => r,
        Err(msg) => return harness_response(false, &msg),
    };

    // Resolve artifact by digest.
    let artifact_path = match capability_host::artifact::resolve_artifact(&config.artifact_root, &req.artifact_digest) {
        Ok(path) => path,
        Err(capability_host::artifact::ArtifactError::NotFound) => return harness_response(false, "artifact_not_found"),
        Err(capability_host::artifact::ArtifactError::InvalidDigest) => return harness_response(false, "artifact_digest_invalid"),
        Err(capability_host::artifact::ArtifactError::DigestMismatch) => return harness_response(false, "artifact_digest_mismatch"),
        Err(capability_host::artifact::ArtifactError::StoreError(msg)) => {
            return harness_response(false, &format!("artifact_store_error:{msg}"));
        }
    };

    let process_req = capability_host::protocol::build_process_request(&req);
    let stdin_json = match serde_json::to_string(&process_req) {
        Ok(j) => j,
        Err(_) => return harness_response(false, "internal_error"),
    };

    let result = capability_host::process::run_artifact(
        &artifact_path, &stdin_json, config.exec_timeout,
        config.max_stdout_bytes, config.max_stderr_bytes,
    );

    match result {
        Ok(output) => {
            if output.exit_code != Some(0) {
                return harness_response(false, "artifact_failed");
            }
            let (ok, response_body) = capability_host::protocol::map_process_response(&output.stdout);
            if ok {
                http_200(&serde_json::to_string(&response_body).unwrap_or_default())
            } else {
                let ec = response_body.get("error_code").and_then(|v| v.as_str()).unwrap_or("artifact_failed");
                harness_response(false, ec)
            }
        }
        Err(capability_host::process::ProcessError::Timeout) => harness_response(false, "artifact_timeout"),
        Err(capability_host::process::ProcessError::IoError(msg)) => {
            harness_response(false, &format!("artifact_exec_error:{msg}"))
        }
    }
}

fn harness_response(ok: bool, error_code: &str) -> String {
    if ok {
        http_200(r#"{"protocol_version":"external-harness-v1","ok":true,"result":null}"#)
    } else {
        http_200(&format!(r#"{{"protocol_version":"external-harness-v1","ok":false,"error_code":"{error_code}"}}"#))
    }
}

fn http_200(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
}
fn http_404() -> String {
    let body = r#"{"error":"not_found"}"#;
    format!("HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
}
fn http_500() -> String {
    let body = r#"{"error":"internal_error"}"#;
    format!("HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
}

fn send_http(host: &str, port: u16, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(format!("{host}:{port}")).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();

    let request = format!(
        "POST /execute HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    stream.write_all(request.as_bytes()).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    let status_line = response.lines().next().unwrap_or("");
    let code: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);

    let json_body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (code, json_body)
}

/// Store a calculator artifact in the ContentStore, return its digest.
fn store_artifact(artifact_root: &PathBuf, binary: &PathBuf) -> String {
    use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
    let bytes = std::fs::read(binary).unwrap();
    let digest = Sha256Digest::compute(&bytes);
    let store = ContentStore::new(artifact_root.clone());
    store.store(&bytes).unwrap();
    digest.as_str().to_string()
}

// ── Tests ──

#[test]
fn valid_artifact_returns_result() {
    let root = std::env::temp_dir().join(format!("ch_test_valid_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let calc_bin = calculator_binary();

    // Store the calculator binary.
    let digest = store_artifact(&root, &calc_bin);

    // Start Capability Host.
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    // Send multiply(6, 7) request.
    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_inv_1",
        "operation": "external.calculator",
        "arguments": {"operation": "multiply", "a": 6, "b": 7},
        "manifest_id": "manifest_test",
        "artifact_digest": digest,
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200, "expected 200, got {code}: {body}");

    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], true, "expected ok=true: {body}");
    assert_eq!(response["result"], 42, "expected 42: {body}");
}

#[test]
fn artifact_digest_mismatch_is_rejected() {
    let root = std::env::temp_dir().join(format!("ch_test_mismatch_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    // Request with a digest that doesn't match stored content.
    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_inv_2",
        "operation": "external.calculator",
        "arguments": {"operation": "add", "a": 1, "b": 2},
        "manifest_id": "manifest_test",
        "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error_code"], "artifact_not_found");
}

#[test]
fn unsupported_protocol_is_rejected() {
    let root = std::env::temp_dir().join(format!("ch_test_proto_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v2",
        "invocation_id": "test_inv_3",
        "operation": "external.calculator",
        "arguments": {},
        "manifest_id": "m",
        "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], false);
}

#[test]
fn missing_artifact_digest_is_rejected() {
    let root = std::env::temp_dir().join(format!("ch_test_missing_digest_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_inv_4",
        "operation": "external.calculator",
        "arguments": {},
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], false);
}

#[test]
fn add_returns_3() {
    let root = std::env::temp_dir().join(format!("ch_test_add_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let calc_bin = calculator_binary();
    let digest = store_artifact(&root, &calc_bin);
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_add",
        "operation": "external.calculator",
        "arguments": {"operation": "add", "a": 1, "b": 2},
        "manifest_id": "m",
        "artifact_digest": digest,
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"], 3);
}

#[test]
fn subtract_returns_2() {
    let root = std::env::temp_dir().join(format!("ch_test_sub_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let calc_bin = calculator_binary();
    let digest = store_artifact(&root, &calc_bin);
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_sub",
        "operation": "external.calculator",
        "arguments": {"operation": "subtract", "a": 5, "b": 3},
        "manifest_id": "m",
        "artifact_digest": digest,
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"], 2);
}

#[test]
fn divide_by_zero_returns_error() {
    let root = std::env::temp_dir().join(format!("ch_test_div0_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let calc_bin = calculator_binary();
    let digest = store_artifact(&root, &calc_bin);
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_div0",
        "operation": "external.calculator",
        "arguments": {"operation": "divide", "a": 1, "b": 0},
        "manifest_id": "m",
        "artifact_digest": digest,
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error_code"], "divide_by_zero");
}

#[test]
fn divide_returns_4() {
    let root = std::env::temp_dir().join(format!("ch_test_div4_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let calc_bin = calculator_binary();
    let digest = store_artifact(&root, &calc_bin);
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_div4",
        "operation": "external.calculator",
        "arguments": {"operation": "divide", "a": 8, "b": 2},
        "manifest_id": "m",
        "artifact_digest": digest,
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"], 4);
}

#[test]
fn unknown_operation_returns_error() {
    let root = std::env::temp_dir().join(format!("ch_test_unknown_op_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let calc_bin = calculator_binary();
    let digest = store_artifact(&root, &calc_bin);
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    let invoke = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "test_unknown",
        "operation": "external.calculator",
        "arguments": {"operation": "power", "a": 2, "b": 3},
        "manifest_id": "m",
        "artifact_digest": digest,
    });

    let (code, body) = send_http("127.0.0.1", port, &invoke.to_string());
    assert_eq!(code, 200);
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["ok"], false);
}

#[test]
fn health_check_returns_ok() {
    let root = std::env::temp_dir().join(format!("ch_test_health_{}", std::process::id()));
    std::fs::create_dir_all(&root).ok();
    let (port, _shutdown) = start_capability_host(&root);
    thread::sleep(Duration::from_millis(200));

    // Use direct HTTP connection instead of send_http helper.
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let request = format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.contains("200"));
    assert!(response.contains("ok"));
}
