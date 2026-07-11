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
        hcr_profiles: std::collections::HashMap::new(),
        hcr_token: String::new(),
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

// ── Capability Proposal Success TCP E2E ──

#[test]
fn capability_proposal_success_over_real_tcp() {
    // Start a mock Kernel API that captures and validates the request.
    let kernel_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let kernel_port = kernel_listener.local_addr().unwrap().port();
    let kernel_url = format!("http://127.0.0.1:{kernel_port}");
    let submit_token = "test-submit-token-e2e".to_string();
    let captured = Arc::new(std::sync::Mutex::new(String::new()));

    let cap = captured.clone();
    let tok = submit_token.clone();
    std::thread::spawn(move || {
        if let Ok(mut s) = kernel_listener.incoming().next().unwrap() {
            // Read all available data (headers + body).
            let mut all = Vec::new();
            let mut tmp = [0u8; 65536];
            loop {
                match s.set_read_timeout(Some(Duration::from_millis(200))) {
                    Ok(()) => match s.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => all.extend_from_slice(&tmp[..n]),
                        Err(_) => break,
                    },
                    Err(_) => break,
                }
            }
            let req = String::from_utf8_lossy(&all);

            // Store captured request for later assertion.
            *cap.lock().unwrap() = req.to_string();

            // Validate Bearer token.
            let auth_header = format!("Bearer {}", tok);
            if !req.contains(&auth_header) {
                let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 28\r\nConnection: close\r\n\r\n{\"error\":\"unauthorized\"}";
                let _ = s.write_all(resp.as_bytes());
                return;
            }

            // Return a proposal response.
            let resp_body = r#"{
                "proposal_id": "proposal_e2e_success",
                "status": "PendingApproval",
                "expected_active_snapshot_id": "snap_e2e_0001",
                "requested_operations": ["external.calculator"],
                "expires_at": "2027-06-01T00:00:00+00:00"
            }"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                resp_body.len(), resp_body
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    // Start coding-harness server.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ws_root = std::env::temp_dir().join(format!("ch_suc_{}_{}", std::process::id(), ts));
    std::fs::create_dir_all(&ws_root).unwrap();
    let artifact_root =
        std::env::temp_dir().join(format!("ch_suc_art_{}_{}", std::process::id(), ts));
    std::fs::create_dir_all(&artifact_root).unwrap();

    // Write fixture files.
    std::fs::write(ws_root.join("artifact.bin"), b"calculator artifact content").unwrap();
    let manifest = serde_json::json!({
        "harness_id": "calc_harness",
        "protocol_version": "external-harness-v1",
        "endpoint": "http://127.0.0.1:9999/execute",
        "operation_name": "external.calculator",
        "description": "Calculator harness E2E",
        "input_schema": {"type":"object","properties":{"operation":{"type":"string"},"a":{"type":"number"},"b":{"type":"number"}},"required":["operation","a","b"],"additionalProperties":false},
        "output_schema": {"type":"object","properties":{"result":{"type":"number"}},"required":["result"],"additionalProperties":false},
        "idempotent": true,
        "target_agent_id": "main",
        "risk_summary": "read-only arithmetic",
    });
    std::fs::write(
        ws_root.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        ws_root.join("evidence.json"),
        r#"{"test":"passed","coverage":100}"#,
    )
    .unwrap();

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
        kernel_api_url: kernel_url,
        capability_submit_token: submit_token.clone(),
        artifact_root: artifact_root.clone(),
        hcr_profiles: std::collections::HashMap::new(),
        hcr_token: String::new(),
    };

    let config = Arc::new(config);
    let _sd = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || coding_harness::server::serve(listener, config));
    std::thread::sleep(Duration::from_millis(200));

    // Send propose request.
    let body = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "operation": "external.coding_capability_propose",
        "arguments": {
            "workspace_id": "test",
            "artifact_path": "artifact.bin",
            "manifest_path": "manifest.json",
            "evidence_path": "evidence.json",
        },
    });
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let body_str = serde_json::to_string(&body).unwrap();
    let request = format!(
        "POST /execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n{}",
        body_str.len(), body_str
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let response = String::from_utf8_lossy(&buf);
    let json_body: serde_json::Value = if let Some(body_start) = response.find("\r\n\r\n") {
        let body = &response[body_start + 4..];
        serde_json::from_str(body).unwrap_or_default()
    } else {
        panic!("no HTTP body")
    };

    eprintln!(
        "Proposal success response: {}",
        serde_json::to_string_pretty(&json_body).unwrap_or_default()
    );

    // Assert response.
    assert_eq!(
        json_body["ok"], true,
        "proposal must return ok:true; got: {json_body}"
    );
    let result = &json_body["result"];
    assert_eq!(result["proposal_id"], "proposal_e2e_success");
    assert_eq!(result["status"], "PendingApproval");
    assert!(result["artifact_digest"]
        .as_str()
        .unwrap_or("")
        .starts_with("sha256:"));
    assert!(result["manifest_digest"]
        .as_str()
        .unwrap_or("")
        .starts_with("sha256:"));
    assert!(result["evidence_digest"]
        .as_str()
        .unwrap_or("")
        .starts_with("sha256:"));
    assert!(
        !result["manifest_id"].as_str().unwrap_or("").is_empty(),
        "manifest_id must be set"
    );
    assert_eq!(result["operation_name"], "external.calculator");
    assert_eq!(
        result["requested_operations"],
        json!(["external.calculator"])
    );

    // Verify captured kernel request.
    let captured_req = captured.lock().unwrap().clone();
    assert!(
        captured_req.contains("Bearer"),
        "must have Authorization header"
    );
    assert!(
        captured_req.contains(&submit_token),
        "must have submit token"
    );
    assert!(
        !captured_req.contains("decision_token"),
        "must NOT have decision token"
    );
    eprintln!("CAPTURED: {}", captured_req);
    assert!(
        captured_req.contains("target_agent_id") || captured_req.contains("\"target_agent_id\""),
        "must pass target_agent_id"
    );
    assert!(
        captured_req.contains("\"external.calculator\""),
        "must pass requested_operations"
    );

    // Cleanup.
    let _ = std::fs::remove_dir_all(&ws_root);
    let _ = std::fs::remove_dir_all(&artifact_root);
}

// ── Symlink escape tests ──

#[test]
fn capability_proposal_artifact_symlink_escape_rejected() {
    let hs = start_basic_harness();
    // Create a symlink pointing outside the workspace.
    let outside_file = hs.ws_root.parent().unwrap().join("outside_artifact.txt");
    std::fs::write(&outside_file, b"outside content").unwrap();
    let symlink = hs.ws_root.join("evil_link.bin");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside_file, &symlink).unwrap();
    }

    let (_, body) = hs.request(
        "external.coding_capability_propose",
        &json!({
            "workspace_id": "test",
            "artifact_path": "evil_link.bin",
            "manifest_path": "manifest.json",
            "evidence_path": "evidence.json",
        }),
    );
    // Symlink escape should be detected.
    assert_eq!(body["ok"], false, "symlink should be rejected; got: {body}");
}

#[test]
fn capability_proposal_manifest_symlink_escape_rejected() {
    let hs = start_basic_harness();
    let outside_file = hs.ws_root.parent().unwrap().join("outside_manifest.txt");
    std::fs::write(&outside_file, b"{}").unwrap();
    let symlink = hs.ws_root.join("evil_manifest.json");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside_file, &symlink).unwrap();
    }

    let (_, body) = hs.request(
        "external.coding_capability_propose",
        &json!({
            "workspace_id": "test",
            "artifact_path": "artifact.bin",
            "manifest_path": "evil_manifest.json",
            "evidence_path": "evidence.json",
        }),
    );
    assert_eq!(
        body["ok"], false,
        "symlink manifest should be rejected; got: {body}"
    );
}

#[test]
fn capability_proposal_evidence_symlink_escape_rejected() {
    let hs = start_basic_harness();
    let outside_file = hs.ws_root.parent().unwrap().join("outside_evidence.txt");
    std::fs::write(&outside_file, b"{}").unwrap();
    let symlink = hs.ws_root.join("evil_evidence.json");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside_file, &symlink).unwrap();
    }

    let (_, body) = hs.request(
        "external.coding_capability_propose",
        &json!({
            "workspace_id": "test",
            "artifact_path": "artifact.bin",
            "manifest_path": "manifest.json",
            "evidence_path": "evil_evidence.json",
        }),
    );
    assert_eq!(
        body["ok"], false,
        "symlink evidence should be rejected; got: {body}"
    );
}
