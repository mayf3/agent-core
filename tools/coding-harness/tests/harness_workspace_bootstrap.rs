//! Integration tests for the external Harness workspace bootstrap.
//!
//! Verifies that the Coding Harness can operate on a "harness-dev" workspace
//! configured similarly to ~/.agent-core/harnesses/.
//!
//! Covers workspace operations, path traversal rejection, and symlink escape
//! rejection when targeting the harness-dev workspace.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Test helpers ──

struct HarnessDevServer {
    port: u16,
    _shutdown: Arc<AtomicBool>,
    ws_root: PathBuf,
    artifact_root: PathBuf,
}

impl HarnessDevServer {
    fn start() -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let ws_root = std::env::temp_dir().join(format!("ch_hd_{}_{}", std::process::id(), ts));
        std::fs::create_dir_all(&ws_root).unwrap();
        let artifact_root =
            std::env::temp_dir().join(format!("ch_hd_art_{}_{}", std::process::id(), ts));
        std::fs::create_dir_all(&artifact_root).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let ws_config = format!(
            r#"{{"workspaces":{{"harness-dev":{{"root":"{}","read":true,"write":true,"exec":true,"opencode":true,"network":true,"shell":false}}}}}}"#,
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
            hcr_profiles: std::collections::HashMap::new(),
            hcr_token: String::new(),
        };

        let config = Arc::new(coding_config);
        let shutdown = Arc::new(AtomicBool::new(false));
        let sd = shutdown.clone();
        std::thread::spawn(move || {
            coding_harness::server::serve(listener, config);
        });

        // Wait for the server to be ready by polling TCP connect.
        // This is more reliable than a fixed sleep.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", port).parse().unwrap(),
                Duration::from_millis(50),
            ) {
                Ok(_) => break,
                Err(e) => {
                    if std::time::Instant::now() >= deadline {
                        panic!(
                            "server did not become ready within 2s on port {}: {}",
                            port, e
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }

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

        let json_body: serde_json::Value = if let Some(body_start) = response.find("\r\n\r\n") {
            let body = &response[body_start + 4..];
            serde_json::from_str(body).unwrap_or(serde_json::json!({"parse_error": body}))
        } else {
            serde_json::json!({"no_body": true})
        };

        (status_code, json_body)
    }
}

impl Drop for HarnessDevServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.ws_root);
        let _ = std::fs::remove_dir_all(&self.artifact_root);
    }
}

// ── Harness workspace bootstrap tests ──

#[test]
fn harness_dev_workspace_write_read_list_exec() {
    let hs = HarnessDevServer::start();

    // Write a file.
    let (code, body) = hs.request(
        "external.coding_workspace_write",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": "hello.txt",
            "content": "Hello from harness-dev!",
            "mode": "replace",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true, "write failed: {body}");
    assert!(hs.ws_root.join("hello.txt").is_file());

    // Read the file back.
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": "hello.txt",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true, "read failed: {body}");
    assert_eq!(body["result"]["content"], "Hello from harness-dev!");

    // List the workspace root.
    let (code, body) = hs.request(
        "external.coding_workspace_list",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": ".",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true, "list failed: {body}");
    let entries = body["result"]["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["name"] == "hello.txt"));

    // Exec a simple command.
    let (code, body) = hs.request(
        "external.coding_workspace_exec",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "command": "echo",
            "args": ["harness-dev-ok"],
            "relative_cwd": ".",
            "timeout_seconds": 10,
            "max_output_bytes": 4096,
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], true, "exec failed: {body}");
    assert_eq!(body["result"]["exit_code"], 0);
    assert!(
        body["result"]["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("harness-dev-ok"),
        "stdout: {:?}",
        body["result"]["stdout"]
    );
}

#[test]
fn harness_dev_workspace_path_traversal_rejected() {
    let hs = HarnessDevServer::start();

    // Absolute path.
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": "/etc/passwd",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "absolute_path_not_allowed");

    // Parent traversal.
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": "../../../etc/passwd",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "path_traversal_not_allowed");

    // Write targeting outside workspace via parent traversal.
    // The `..` component is caught by validate_relative before the boundary check.
    let (code, body) = hs.request(
        "external.coding_workspace_write",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": "../outside.txt",
            "content": "stolen",
            "mode": "replace",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "path_traversal_not_allowed");
}

#[test]
#[cfg(unix)]
fn harness_dev_workspace_symlink_escape_rejected() {
    use std::os::unix::fs::symlink;
    let hs = HarnessDevServer::start();

    // Create a file outside the workspace.
    let outside_file = hs.ws_root.parent().unwrap().join("outside_sym_test.txt");
    std::fs::write(&outside_file, b"sensitive data outside workspace").unwrap();

    // Create a symlink inside the workspace pointing outside.
    let symlink_path = hs.ws_root.join("escape_lnk.txt");
    symlink(&outside_file, &symlink_path).unwrap();

    // Read through the symlink should be rejected.
    // resolve_path canonicalizes the symlink, discovering it points outside,
    // and returns path_outside_workspace.
    let (code, body) = hs.request(
        "external.coding_workspace_read",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": "escape_lnk.txt",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(
        body["ok"], false,
        "expected symlink read to be rejected: {body}"
    );
    assert!(
        body["error_code"].as_str().unwrap_or("") == "path_outside_workspace"
            || body["error_code"].as_str().unwrap_or("") == "path_not_found",
        "unexpected error_code: {:?}",
        body["error_code"]
    );

    // Write through a symlink should also be rejected.
    let symlink_write = hs.ws_root.join("write_target_link");
    symlink(&outside_file, &symlink_write).unwrap();
    let (code, body) = hs.request(
        "external.coding_workspace_write",
        &serde_json::json!({
            "workspace_id": "harness-dev",
            "relative_path": "write_target_link",
            "content": "attempted write through symlink",
            "mode": "replace",
        }),
    );
    assert_eq!(code, 200);
    assert_eq!(
        body["ok"], false,
        "expected symlink write to be rejected: {body}"
    );
}
