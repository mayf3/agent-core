//! Capability Proposal TCP E2E and path validation tests.
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct HarnessServer {
    port: u16,
    _shutdown: Arc<AtomicBool>,
    ws_root: PathBuf,
    artifact_root: PathBuf,
}

impl HarnessServer {
    fn request(&self, operation: &str, args: &serde_json::Value) -> (u16, serde_json::Value) {
        let body = json!({
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
            serde_json::from_str(body).unwrap_or(json!({"parse_error": body}))
        } else {
            json!({"no_body": true})
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

fn start_basic_harness() -> HarnessServer {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ws_root = std::env::temp_dir().join(format!("ch_cap_{}_{}", std::process::id(), ts));
    std::fs::create_dir_all(&ws_root).unwrap();
    let artifact_root =
        std::env::temp_dir().join(format!("ch_cap_art_{}_{}", std::process::id(), ts));
    std::fs::create_dir_all(&artifact_root).unwrap();
    std::fs::write(ws_root.join("artifact.bin"), b"test artifact").unwrap();
    std::fs::write(
        ws_root.join("manifest.json"),
        r#"{"operation_name":"test.op"}"#,
    )
    .unwrap();
    std::fs::write(ws_root.join("evidence.json"), r#"{}"#).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = coding_harness::config::CodingConfig {
        workspaces: {
            let mut map = std::collections::HashMap::new();
            let perm = coding_harness::config::WorkspacePermission {
                read: true,
                write: true,
                exec: true,
                opencode: true,
                network: true,
                shell: false,
            };
            map.insert(
                "test".to_string(),
                coding_harness::config::WorkspaceEntry {
                    root: std::fs::canonicalize(&ws_root).unwrap_or_else(|_| ws_root.clone()),
                    perm,
                },
            );
            map
        },
        kernel_api_url: "http://127.0.0.1:1".into(),
        capability_submit_token: "test-token".into(),
        artifact_root: artifact_root.clone(),
    };
    let config = Arc::new(config);
    let shutdown = Arc::new(AtomicBool::new(false));
    let sd = shutdown.clone();
    std::thread::spawn(move || coding_harness::server::serve(listener, config));
    std::thread::sleep(Duration::from_millis(100));
    HarnessServer {
        port,
        _shutdown: sd,
        ws_root,
        artifact_root,
    }
}

#[test]
fn capability_proposal_absolute_path_rejected() {
    let hs = start_basic_harness();
    let (_, body) = hs.request(
        "external.coding_capability_propose",
        &json!({
            "workspace_id": "test", "artifact_path": "/etc/passwd",
            "manifest_path": "manifest.json", "evidence_path": "evidence.json",
        }),
    );
    assert_eq!(body["ok"], false);
    assert!(body["error_code"]
        .as_str()
        .unwrap_or("")
        .contains("absolute_path"));
}

#[test]
fn capability_proposal_parent_traversal_rejected() {
    let hs = start_basic_harness();
    let (_, body) = hs.request(
        "external.coding_capability_propose",
        &json!({
            "workspace_id": "test", "artifact_path": "../../../etc/passwd",
            "manifest_path": "manifest.json", "evidence_path": "evidence.json",
        }),
    );
    assert_eq!(body["ok"], false);
    assert!(body["error_code"]
        .as_str()
        .unwrap_or("")
        .contains("path_traversal"));
}

#[test]
fn capability_proposal_unknown_workspace_rejected() {
    let hs = start_basic_harness();
    let (_, body) = hs.request(
        "external.coding_capability_propose",
        &json!({
            "workspace_id": "nonexistent", "artifact_path": "artifact.bin",
            "manifest_path": "manifest.json", "evidence_path": "evidence.json",
        }),
    );
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "unknown_workspace_id");
}

#[test]
fn capability_proposal_missing_paths_rejected() {
    let hs = start_basic_harness();
    let (_, body) = hs.request(
        "external.coding_capability_propose",
        &json!({
            "workspace_id": "test", "artifact_path": "",
            "manifest_path": "", "evidence_path": "",
        }),
    );
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "missing_path");
}
