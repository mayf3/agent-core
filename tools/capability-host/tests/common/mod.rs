//! Shared helpers for Capability Host integration tests.
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

/// Locate the calculator artifact binary (built as part of coding-harness).
pub fn calculator_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut p = exe.parent()?;
    loop {
        let name = p.file_name()?;
        if name == "target" {
            let profile = if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            };
            let mut ws = p.parent()?;
            ws = ws.parent()?;
            ws = ws.parent()?;
            let ch_target = ws
                .join("tools")
                .join("coding-harness")
                .join("target")
                .join(profile)
                .join("calculator-artifact");
            if ch_target.exists() {
                return Some(ch_target);
            }
            let ws_target = p.join(profile).join("calculator-artifact");
            if ws_target.exists() {
                return Some(ws_target);
            }
            break;
        }
        p = p.parent()?;
    }
    None
}

/// Start Capability Host on a random port using the production server.
pub fn start_capability_host(artifact_root: &PathBuf) -> (u16, Arc<AtomicBool>) {
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
        // Use the production server's handle function
        for stream in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(stream) = stream {
                capability_host::server::handle(stream, &config);
            }
        }
    });
    (port, shutdown)
}

pub fn send_http(host: &str, port: u16, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(format!("{host}:{port}")).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let request = format!("POST /execute HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
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

pub fn store_artifact(artifact_root: &PathBuf, binary: &PathBuf) -> String {
    use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
    let bytes = std::fs::read(binary).unwrap();
    let digest = Sha256Digest::compute(&bytes);
    ContentStore::new(artifact_root.clone())
        .store(&bytes)
        .unwrap();
    digest.as_str().to_string()
}
