//! End-to-end protocol tests: spawn the real binary, drive it over HTTP
//! exactly as the Kernel external-harness adapter does.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

/// Kills the spawned harness even when a test panics, so a failed run never
/// leaks a listener that would shadow later test runs on the same port.
struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
use std::time::Duration;

fn port_for(name: &str) -> u16 {
    // Deterministic per-test ports derived from the test name hash.
    let mut h: u32 = 0x811c9dc5;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    20000 + (h % 20000) as u16
}

fn spawn_harness(name: &str, root: &str) -> (KillOnDrop, u16) {
    let port = port_for(name);
    let child = Command::new(env!("CARGO_BIN_EXE_execution-harness"))
        .env("EXECUTION_HARNESS_LISTEN_ADDR", format!("127.0.0.1:{port}"))
        .env("EXECUTION_HARNESS_TOKEN", "test-token-0123456789")
        .env("EXECUTION_HARNESS_WORKSPACE_ROOT", root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn harness");
    // Wait for the listener.
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return (KillOnDrop(child), port);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("harness did not start on port {port}");
}

fn post(port: u16, token: Option<&str>, body: &Value) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let bytes = serde_json::to_vec(body).unwrap();
    let auth = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    let req = format!(
        "POST /execute HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.write_all(&bytes).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw);
    let status: u16 = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body_str = text.split("\r\n\r\n").nth(1).unwrap_or("");
    let parsed: Value = serde_json::from_str(body_str).unwrap_or(json!({}));
    (status, parsed)
}

fn args(op: &str, arguments: Value) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "invocation_id": "inv_test",
        "operation": op,
        "arguments": arguments,
    })
}

#[test]
fn auth_fail_closed() {
    let root = std::env::temp_dir().join("eh-auth-test");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (mut child, port) = spawn_harness("auth", root.to_str().unwrap());

    let (s1, b1) = post(port, None, &args("external.coding_workspace_list", json!({"workspace_id": "ws"})));
    assert_eq!(s1, 401, "missing token must 401: {b1}");
    let (s2, b2) = post(port, Some("wrong-token"), &args("external.coding_workspace_list", json!({"workspace_id": "ws"})));
    assert_eq!(s2, 401, "wrong token must 401: {b2}");
    let (s3, b3) = post(port, Some("test-token-0123456789"), &args("external.coding_workspace_list", json!({"workspace_id": "ws"})));
    assert_eq!(s3, 200, "valid token reaches handler: {b3}");
    assert_eq!(b3["ok"], true);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn write_read_exec_roundtrip() {
    let root = std::env::temp_dir().join("eh-wre-test");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (mut child, port) = spawn_harness("wre", root.to_str().unwrap());
    let tok = Some("test-token-0123456789");

    // write
    let (s, b) = post(port, tok, &args(
        "external.coding_workspace_write",
        json!({"workspace_id": "demo", "relative_path": "hello.txt", "content": "hi there"}),
    ));
    assert_eq!(s, 200);
    assert_eq!(b["ok"], true);

    // read back
    let (s, b) = post(port, tok, &args(
        "external.coding_workspace_read",
        json!({"workspace_id": "demo", "relative_path": "hello.txt"}),
    ));
    assert_eq!(s, 200);
    assert_eq!(b["result"]["content"], "hi there");

    // list
    let (s, b) = post(port, tok, &args(
        "external.coding_workspace_list",
        json!({"workspace_id": "demo"}),
    ));
    assert_eq!(s, 200);
    assert_eq!(b["result"]["entry_count"], 1);

    // exec: cat the file via shell
    let (s, b) = post(port, tok, &args(
        "external.coding_workspace_exec",
        json!({"workspace_id": "demo", "command": "cat", "args": ["hello.txt"], "timeout_seconds": 10}),
    ));
    assert_eq!(s, 200);
    assert_eq!(b["result"]["exit_code"], 0);
    assert_eq!(b["result"]["stdout"], "hi there");

    // path escape rejected (ok=false, 200 status)
    let (s, b) = post(port, tok, &args(
        "external.coding_workspace_read",
        json!({"workspace_id": "demo", "relative_path": "../../../etc/passwd"}),
    ));
    assert_eq!(s, 200);
    assert_eq!(b["ok"], false);
    assert_eq!(b["error_code"], "path_escape");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn exec_timeout_kills() {
    let root = std::env::temp_dir().join("eh-timeout-test");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (mut child, port) = spawn_harness("timeout", root.to_str().unwrap());
    let tok = Some("test-token-0123456789");
    let start = std::time::Instant::now();
    let (s, b) = post(port, tok, &args(
        "external.coding_workspace_exec",
        json!({"workspace_id": "ws", "command": "sh", "args": ["-c", "sleep 60"], "timeout_seconds": 2}),
    ));
    assert_eq!(s, 200);
    assert_eq!(b["result"]["timed_out"], true);
    assert!(start.elapsed() < Duration::from_secs(15), "must not wait 60s");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unknown_operation_and_protocol_rejected() {
    let root = std::env::temp_dir().join("eh-unknown-test");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (mut child, port) = spawn_harness("unknown", root.to_str().unwrap());
    let tok = Some("test-token-0123456789");

    let (s, b) = post(port, tok, &json!({
        "protocol_version": "external-harness-v1",
        "operation": "external.some_unknown_op",
        "arguments": {},
    }));
    assert_eq!(s, 200);
    assert_eq!(b["error_code"], "unknown_operation");

    let (s, _) = post(port, tok, &json!({
        "protocol_version": "wrong-version",
        "operation": "external.coding_workspace_list",
        "arguments": {},
    }));
    assert_eq!(s, 400, "unsupported protocol must 400");

    let _ = std::fs::remove_dir_all(&root);
}
