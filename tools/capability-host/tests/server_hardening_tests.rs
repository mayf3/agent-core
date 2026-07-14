mod common;
use common::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn root(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "capability_host_server_{label}_{}_{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn oversized_and_transfer_encoded_headers_are_rejected() {
    let root = root("headers");
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(Duration::from_millis(100));
    let oversized = format!(
        "GET /health HTTP/1.1\r\nX-Oversized: {}\r\n\r\n",
        "a".repeat(5000)
    );
    let response = send_raw(port, oversized.as_bytes(), Duration::from_secs(2));
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");

    let encoded = b"POST /execute HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
    let response = send_raw(port, encoded, Duration::from_secs(2));
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
}

#[test]
fn incomplete_header_is_bounded_by_read_timeout() {
    let root = root("timeout");
    let (port, _) = start_capability_host(&root);
    std::thread::sleep(Duration::from_millis(100));
    let started = std::time::Instant::now();
    let response = send_raw(
        port,
        b"GET /health HTTP/1.1\r\nX-Incomplete: waiting",
        Duration::from_secs(7),
    );
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    assert!(started.elapsed() < Duration::from_secs(7));
}

#[test]
fn production_server_enforces_concurrency_limit() {
    let root = root("concurrency");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = capability_host::config::CapabilityHostConfig {
        listen_addr: format!("127.0.0.1:{port}"),
        artifact_root: root,
        exec_timeout: Duration::from_secs(3),
        max_stdout_bytes: 65536,
        max_stderr_bytes: 65536,
        control_token: CONTROL_TOKEN.into(),
        execution_token: EXECUTION_TOKEN.into(),
    };
    std::thread::spawn(move || capability_host::server::serve_listener(config, listener));
    std::thread::sleep(Duration::from_millis(200));

    let mut held = Vec::new();
    for _ in 0..32 {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nX-Hold: waiting")
            .unwrap();
        held.push(stream);
    }
    std::thread::sleep(Duration::from_millis(200));
    let response = send_raw(
        port,
        b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
        Duration::from_secs(2),
    );
    assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    drop(held);
}

#[test]
fn config_rejects_non_loopback_weak_or_shared_tokens() {
    let base = || capability_host::config::CapabilityHostConfig {
        listen_addr: "127.0.0.1:7300".into(),
        artifact_root: root("config"),
        exec_timeout: Duration::from_secs(3),
        max_stdout_bytes: 65536,
        max_stderr_bytes: 65536,
        control_token: CONTROL_TOKEN.into(),
        execution_token: EXECUTION_TOKEN.into(),
    };
    let mut non_loopback = base();
    non_loopback.listen_addr = "0.0.0.0:7300".into();
    assert!(non_loopback.validate().is_err());
    let mut weak = base();
    weak.control_token = "short".into();
    assert!(weak.validate().is_err());
    let mut shared = base();
    shared.execution_token = shared.control_token.clone();
    assert!(shared.validate().is_err());
}

#[test]
fn legacy_shared_token_environment_is_not_a_fallback() {
    let current = std::env::current_exe().unwrap();
    let binary = current
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("capability-host");
    assert!(binary.is_file(), "binary missing at {}", binary.display());
    let output = std::process::Command::new(binary)
        .env_clear()
        .env("CAPABILITY_HOST_ARTIFACT_ROOT", root("no-fallback"))
        .env(
            "CAPABILITY_HOST_TOKEN",
            "legacy-shared-token-that-must-not-be-accepted",
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("CONTROL_TOKEN is required"));
}

fn send_raw(port: u16, request: &[u8], timeout: Duration) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.write_all(request).unwrap();
    let mut response = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(error) => panic!("read response failed: {error}"),
        }
    }
    String::from_utf8(response).unwrap()
}
