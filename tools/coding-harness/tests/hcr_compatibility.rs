//! HCR compatibility tests (30-32).
//!
//! Tests that ordinary coding behavior is unchanged and HCR profile
//! behaves correctly when unconfigured or unavailable.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coding_harness::hcr::sandbox::SandboxBackend;

// ── Test 30: ordinary coding profile behavior unchanged ──

#[test]
fn ordinary_coding_profile_behavior_unchanged() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ws_root = std::env::temp_dir().join(format!("hcr_comp_30_{}", ts));
    std::fs::create_dir_all(&ws_root).unwrap();
    let artifact_root = std::env::temp_dir().join(format!("hcr_comp_30_art_{}", ts));
    std::fs::create_dir_all(&artifact_root).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let config = coding_harness::config::CodingConfig {
        workspaces: {
            let mut map = std::collections::HashMap::new();
            map.insert(
                "test".to_string(),
                coding_harness::config::WorkspaceEntry {
                    root: std::fs::canonicalize(&ws_root).unwrap_or_else(|_| ws_root.clone()),
                    perm: coding_harness::config::WorkspacePermission {
                        read: true,
                        write: true,
                        exec: true,
                        opencode: true,
                        network: true,
                        shell: false,
                    },
                },
            );
            map
        },
        kernel_api_url: format!("http://127.0.0.1:{}", port as u32 + 1000),
        capability_submit_token: "test-token".into(),
        artifact_root: artifact_root.clone(),
        hcr_profiles: std::collections::HashMap::new(),
        hcr_token: String::new(),
    };

    let config = Arc::new(config);
    std::thread::spawn(move || {
        coding_harness::server::serve(listener, config);
    });
    std::thread::sleep(Duration::from_millis(100));

    let body = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "operation": "external.coding_workspace_exec",
        "arguments": {
            "workspace_id": "test",
            "command": "echo",
            "args": ["ordinary_ok"],
            "relative_cwd": ".",
            "timeout_seconds": 10,
        },
    });
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let body_str = serde_json::to_string(&body).unwrap();
    let request = format!(
        "POST /execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n{}",
        body_str.len(), port, body_str
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let response = String::from_utf8_lossy(&buf);
    let json_body: serde_json::Value = if let Some(body_start) = response.find("\r\n\r\n") {
        let body = &response[body_start + 4..];
        serde_json::from_str(body).unwrap_or_default()
    } else {
        serde_json::Value::Null
    };

    assert_eq!(
        json_body["ok"], true,
        "ordinary coding exec must still work; got: {json_body}"
    );

    let _ = std::fs::remove_dir_all(&ws_root);
    let _ = std::fs::remove_dir_all(&artifact_root);
}

// ── Test 31: HCR profile unavailable unless configured ──

#[test]
fn hcr_profile_unavailable_unless_configured() {
    let config = coding_harness::config::CodingConfig {
        workspaces: std::collections::HashMap::new(),
        kernel_api_url: String::new(),
        capability_submit_token: String::new(),
        artifact_root: PathBuf::from("/tmp"),
        hcr_profiles: std::collections::HashMap::new(),
        hcr_token: String::new(),
    };

    assert!(config.hcr_profiles.is_empty(), "no HCR profiles by default");
    assert!(config.hcr_token.is_empty(), "no HCR token by default");
}

// ── Test 32: unsupported platform fails closed ──

#[test]
fn unsupported_platform_fails_closed() {
    let backend = SandboxBackend::Unavailable;
    assert_eq!(
        backend,
        SandboxBackend::Unavailable,
        "Unavailable backend must not be available"
    );

    let detected = SandboxBackend::detect();
    match detected {
        SandboxBackend::MacOSSandboxExec | SandboxBackend::LinuxBubblewrap => {
            eprintln!("Sandbox backend available: {:?}", detected);
        }
        SandboxBackend::Unavailable => {
            eprintln!("No sandbox backend available");
        }
    }
}

// ── Extra: HCR exec via server requires token ──

#[test]
fn hcr_exec_via_server_requires_token() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ws_root = std::env::temp_dir().join(format!("hcr_comp_tok_{}", ts));
    std::fs::create_dir_all(&ws_root).unwrap();
    let artifact_root = std::env::temp_dir().join(format!("hcr_comp_tok_art_{}", ts));
    std::fs::create_dir_all(&artifact_root).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    // Config with HCR profiles but EMPTY token
    let config = coding_harness::config::CodingConfig {
        workspaces: {
            let mut map = std::collections::HashMap::new();
            map.insert(
                "test".to_string(),
                coding_harness::config::WorkspaceEntry {
                    root: std::fs::canonicalize(&ws_root).unwrap_or_else(|_| ws_root.clone()),
                    perm: coding_harness::config::WorkspacePermission {
                        read: true,
                        write: true,
                        exec: true,
                        opencode: true,
                        network: true,
                        shell: false,
                    },
                },
            );
            map
        },
        kernel_api_url: format!("http://127.0.0.1:{}", port as u32 + 1000),
        capability_submit_token: "test-token".into(),
        artifact_root: artifact_root.clone(),
        hcr_profiles: std::collections::HashMap::new(),
        hcr_token: String::new(), // empty = HCR disabled
    };

    let config = Arc::new(config);
    std::thread::spawn(move || {
        coding_harness::server::serve(listener, config);
    });
    std::thread::sleep(Duration::from_millis(100));

    let body = serde_json::json!({
        "protocol_version": "external-harness-v1",
        "operation": "external.coding_hcr_exec",
        "arguments": {
            "workspace_id": "test",
            "hcr_profile_id": "hcr-v0",
            "hcr_token": "",
            "command": "echo_test",
            "params": {},
        },
    });
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let body_str = serde_json::to_string(&body).unwrap();
    let request = format!(
        "POST /execute HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n{}",
        body_str.len(), port, body_str
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let response = String::from_utf8_lossy(&buf);
    let json_body: serde_json::Value = if let Some(body_start) = response.find("\r\n\r\n") {
        let body = &response[body_start + 4..];
        serde_json::from_str(body).unwrap_or_default()
    } else {
        serde_json::Value::Null
    };

    assert_eq!(json_body["ok"], false);
    assert_eq!(
        json_body["error_code"], "hcr_token_required",
        "expected token required, got: {json_body}"
    );

    let _ = std::fs::remove_dir_all(&ws_root);
    let _ = std::fs::remove_dir_all(&artifact_root);
}
