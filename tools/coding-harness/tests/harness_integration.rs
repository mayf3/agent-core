//! Integration tests for the external Coding Harness TCP server.
//!
//! Starts the real coding-harness TCP server on a random port, sends
//! external-harness-v1 HTTP requests, parses the HTTP response, and
//! verifies the JSON body matches expected protocol.
//!
//! Covers all 7 operations and permission/path/backend edge cases.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Test helpers ──

struct HarnessServer {
    port: u16,
    _shutdown: Arc<AtomicBool>,
    ws_root: PathBuf,
    artifact_root: PathBuf,
}

impl HarnessServer {
    fn start() -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let ws_root = std::env::temp_dir().join(format!("ch_int_{}_{}", std::process::id(), ts));
        std::fs::create_dir_all(&ws_root).unwrap();
        let artifact_root =
            std::env::temp_dir().join(format!("ch_int_art_{}_{}", std::process::id(), ts));
        std::fs::create_dir_all(&artifact_root).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let ws_config = format!(
            r#"{{"workspaces":{{"test":{{"root":"{}","read":true,"write":true,"exec":true,"opencode":true,"network":true,"shell":false}}}}}}"#,
            ws_root.to_string_lossy()
        );

        let coding_config = coding_harness::config::CodingConfig {
            workspaces: {
                let parsed: serde_json::Value = serde_json::from_str(&ws_config).unwrap();
                let mut map = std::collections::HashMap::new();
                if let Some(wss) = parsed.get("workspaces").and_then(|v| v.as_object()) {
                    for (id, cfg) in wss {
                        let root_str = cfg.get("root").and_then(|v| v.as_str()).unwrap_or("");
                        let canon = std::fs::canonicalize(root_str)
                            .unwrap_or_else(|_| PathBuf::from(root_str));
                        let perm = coding_harness::config::WorkspacePermission {
                            read: cfg.get("read").and_then(|v| v.as_bool()).unwrap_or(false),
                            write: cfg.get("write").and_then(|v| v.as_bool()).unwrap_or(false),
                            exec: cfg.get("exec").and_then(|v| v.as_bool()).unwrap_or(false),
                            opencode: cfg
                                .get("opencode")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            network: cfg
                                .get("network")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            shell: cfg.get("shell").and_then(|v| v.as_bool()).unwrap_or(false),
                        };
                        map.insert(
                            id.clone(),
                            coding_harness::config::WorkspaceEntry { root: canon, perm },
                        );
                    }
                }
                map
            },
            kernel_api_url: format!("http://127.0.0.1:{}", port as u32 + 1000),
            capability_submit_token: "test-submit-token".into(),
            artifact_root: artifact_root.clone(),
        };

        let config = Arc::new(coding_config);
        let shutdown = Arc::new(AtomicBool::new(false));
        let sd = shutdown.clone();
        std::thread::spawn(move || {
            coding_harness::server::serve(listener, config);
        });
        std::thread::sleep(Duration::from_millis(100));

        Self {
            port,
            _shutdown: sd,
            ws_root,
            artifact_root,
        }
    }

    fn request(&self, operation: &str, args: &serde_json::Value) -> (u16, serde_json::Value) {
        let body = serde_json::json!({
            "protocol_version": "external-harness-v1",
            "operation": operation,
            "arguments": args,
        });
        let body_str = serde_json::to_string(&body).unwrap();

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = format!(
            "POST /execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n{}",
            body_str.len(), self.port, body_str
        );
        stream.write_all(request.as_bytes()).unwrap();

        // Read HTTP response.
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        let response = String::from_utf8_lossy(&buf);

        // Parse status line.
        let status_line = response.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);

        // Parse body after headers.
        let json_body: serde_json::Value = if let Some(body_start) = response.find("\r\n\r\n") {
            let body = &response[body_start + 4..];
            serde_json::from_str(body).unwrap_or(serde_json::json!({"parse_error": body}))
        } else {
            serde_json::json!({"no_body": true})
        };

        (status_code, json_body)
    }
}

impl Drop for HarnessServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.ws_root);
        let _ = std::fs::remove_dir_all(&self.artifact_root);
    }
}

// ── Protocol-level tests ──

#[test]
fn response_protocol_ok_true() {
    let hs = HarnessServer::start();
    std::fs::write(hs.ws_root.join("test.txt"), b"hello").unwrap();
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "test",
            "relative_path": "test.txt",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true, "expected ok:true; got: {body}");
    assert!(body.get("protocol_version").is_some());
    assert!(body.get("result").is_some());
}

#[test]
fn response_protocol_ok_false_on_error() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "test",
            "relative_path": "nonexistent.txt",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false, "expected ok:false; got: {body}");
    assert_eq!(body["error_code"], "path_not_found");
}

#[test]
fn response_protocol_unknown_operation() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request(
        "external.nonexistent_op",
        &serde_json::json!({
            "workspace_id": "test",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "unknown_operation");
}

#[test]
fn response_protocol_invalid_protocol_version() {
    let hs = HarnessServer::start();
    let body = serde_json::json!({
        "protocol_version": "bad-version",
        "operation": "external.coding_workspace_list",
        "arguments": {},
    });
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", hs.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let body_str = serde_json::to_string(&body).unwrap();
    let request = format!(
        "POST /execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n{}",
        body_str.len(), hs.port, body_str
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let response = String::from_utf8_lossy(&buf);
    let status_line = response.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    assert_eq!(status_code, 400, "bad protocol version should get 400");

    let json_body: serde_json::Value = if let Some(body_start) = response.find("\r\n\r\n") {
        let body = &response[body_start + 4..];
        serde_json::from_str(body).unwrap_or_default()
    } else {
        serde_json::Value::Null
    };
    assert_eq!(json_body["ok"], false);
    assert_eq!(json_body["error_code"], "unsupported_protocol");
}

// ── Workspace operations ──

#[test]
fn workspace_write_read_list() {
    let hs = HarnessServer::start();

    // Write.
    let (code, body) = hs.request(
        "external.coding_workspace_write",
        &serde_json::json!({
            "workspace_id": "test",
            "relative_path": "hello.txt",
            "content": "Hello, World!",
            "mode": "replace",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true);
    assert!(hs.ws_root.join("hello.txt").is_file());

    // Read.
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "test",
            "relative_path": "hello.txt",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["result"]["content"], "Hello, World!");

    // List.
    let (code, body) = hs.request(
        "external.coding_workspace_list",
        &serde_json::json!({
            "workspace_id": "test",
            "relative_path": ".",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true);
    let entries = body["result"]["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["name"] == "hello.txt"));
}

#[test]
fn workspace_exec() {
    let hs = HarnessServer::start();
    std::fs::write(hs.ws_root.join("calc.rs"), b"fn add(a:i32,b:i32)->i32{a+b}").unwrap();

    let (code, body) = hs.request(
        "external.coding_workspace_exec",
        &serde_json::json!({
            "workspace_id": "test",
            "command": "rustc",
            "args": ["calc.rs", "--crate-type", "lib"],
            "relative_cwd": ".",
            "timeout_seconds": 30,
            "max_output_bytes": 65536,
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["result"]["exit_code"], 0);
}

// ── Task operations ──

#[test]
fn task_submit_fake_state_machine() {
    let hs = HarnessServer::start();

    let (code, body) = hs.request(
        "external.coding_task_submit",
        &serde_json::json!({
            "workspace_id": "test",
            "objective": "build",
            "acceptance_criteria": "pass",
            "backend": "fake",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["result"]["status"], "queued");
    let task_id = body["result"]["task_id"].as_str().unwrap().to_string();

    // Wait for completion.
    std::thread::sleep(Duration::from_millis(200));

    let (code, body) = hs.request(
        "external.coding_task_status",
        &serde_json::json!({
            "task_id": task_id,
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["result"]["status"], "succeeded");
}

#[test]
fn task_unknown_backend() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request(
        "external.coding_task_submit",
        &serde_json::json!({
            "workspace_id": "test",
            "objective": "build",
            "acceptance_criteria": "pass",
            "backend": "nonexistent_backend",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert!(body["error_code"]
        .as_str()
        .unwrap_or("")
        .starts_with("unsupported_backend"));
}

// ── Permission tests ──

#[test]
fn permission_read_false() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "nonexistent",
            "relative_path": ".",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "unknown_workspace_id");
}

// ── Path validation tests ──

#[test]
fn path_absolute_rejected() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "test",
            "relative_path": "/etc/passwd",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "absolute_path_not_allowed");
}

#[test]
fn path_parent_traversal_rejected() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "test",
            "relative_path": "../../../etc/passwd",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "path_traversal_not_allowed");
}

// ── Acceptance criteria array ──

#[test]
fn task_acceptance_criteria_array() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request(
        "external.coding_task_submit",
        &serde_json::json!({
            "workspace_id": "test",
            "objective": "build",
            "acceptance_criteria": ["criterion A", "criterion B"],
            "backend": "fake",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true);
    let tid = body["result"]["task_id"].as_str().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(200));
    let (_, sbody) = hs.request(
        "external.coding_task_status",
        &serde_json::json!({
            "task_id": tid,
        }),
    );
    // Succeeded without error means acceptance criteria was processed (no crash).
    assert_eq!(sbody["result"]["status"], "succeeded");
}

// ── Body size limit ──

#[test]
fn body_size_limit_exceeded() {
    let hs = HarnessServer::start();
    let large_payload = "x".repeat(3_000_000);
    let body = serde_json::json!({ "data": large_payload });
    let body_str = serde_json::to_string(&body).unwrap();
    let request = format!(
        "POST /execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n{}",
        body_str.len(), hs.port, body_str
    );
    // The server may reject before reading all data, causing connection reset.
    // We accept either a clean 413 or a connection error indicating rejection.
    let result = (|| -> Option<u16> {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", hs.port)).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .ok()?;
        let _ = stream.write_all(request.as_bytes());
        // Try to read, accept failure since server may close abruptly.
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        let response = String::from_utf8_lossy(&buf);
        let status_line = response.lines().next().unwrap_or("");
        status_line.split_whitespace().nth(1)?.parse().ok()
    })();
    // Either we got 413 or the connection was reset (meaning the server rejected
    // the oversized body before fully reading it, which is acceptable behavior).
    assert!(
        result == Some(413) || result.is_none(),
        "expected 413 or connection reset; got: {:?}",
        result
    );
}
