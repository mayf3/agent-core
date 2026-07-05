//! Shared helpers for Capability Host integration tests.

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

/// Start Capability Host on a random port.
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

pub fn handle_request(
    stream: &mut TcpStream,
    config: &capability_host::config::CapabilityHostConfig,
) -> String {
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut rl = String::new();
    if reader.read_line(&mut rl).is_err() {
        return http_500();
    }
    let p: Vec<&str> = rl.split_whitespace().collect();
    if p.len() < 2 {
        return http_500();
    }
    let (method, path) = (p[0], p[1]);
    let mut cl: usize = 0;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        if h.to_ascii_lowercase().starts_with("content-length:") {
            cl = h
                .split(':')
                .nth(1)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
        }
    }
    let mut body = String::new();
    if cl > 0 {
        let mut buf = vec![0u8; cl];
        reader.read_exact(&mut buf).ok();
        body = String::from_utf8(buf).unwrap_or_default();
    }
    match (method, path) {
        ("GET", "/health") => http_200(r#"{"status":"ok"}"#),
        ("POST", "/execute") => execute_artifact(&body, config),
        _ => http_404(),
    }
}

fn execute_artifact(body: &str, config: &capability_host::config::CapabilityHostConfig) -> String {
    let bj: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return harness_resp(false, "malformed_request"),
    };
    let req = match capability_host::protocol::parse_harness_request(&bj) {
        Ok(r) => r,
        Err(m) => return harness_resp(false, &m),
    };
    let ap = match capability_host::artifact::resolve_artifact(
        &config.artifact_root,
        &req.artifact_digest,
    ) {
        Ok(p) => p,
        Err(capability_host::artifact::ArtifactError::NotFound) => {
            return harness_resp(false, "artifact_not_found")
        }
        Err(capability_host::artifact::ArtifactError::InvalidDigest) => {
            return harness_resp(false, "artifact_digest_invalid")
        }
        Err(capability_host::artifact::ArtifactError::DigestMismatch) => {
            return harness_resp(false, "artifact_digest_mismatch")
        }
        Err(capability_host::artifact::ArtifactError::StoreError(m)) => {
            return harness_resp(false, &format!("artifact_store_error:{m}"))
        }
    };
    let sj = serde_json::to_string(&capability_host::protocol::build_process_request(&req))
        .unwrap_or_default();
    match capability_host::process::run_artifact(
        &ap,
        &sj,
        config.exec_timeout,
        config.max_stdout_bytes,
        config.max_stderr_bytes,
    ) {
        Ok(out) => {
            if out.exit_code != Some(0) {
                return harness_resp(false, "artifact_failed");
            }
            let (ok, rb) = capability_host::protocol::map_process_response(&out.stdout);
            if ok {
                http_200(&serde_json::to_string(&rb).unwrap_or_default())
            } else {
                harness_resp(
                    false,
                    rb.get("error_code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("artifact_failed"),
                )
            }
        }
        Err(capability_host::process::ProcessError::Timeout) => {
            harness_resp(false, "artifact_timeout")
        }
        Err(capability_host::process::ProcessError::IoError(m)) => {
            harness_resp(false, &format!("artifact_exec_error:{m}"))
        }
    }
}

pub fn harness_resp(ok: bool, error_code: &str) -> String {
    if ok {
        http_200(r#"{"protocol_version":"external-harness-v1","ok":true,"result":null}"#)
    } else {
        http_200(&format!(
            r#"{{"protocol_version":"external-harness-v1","ok":false,"error_code":"{error_code}"}}"#
        ))
    }
}

pub fn http_200(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}
fn http_404() -> String {
    http_200(r#"{"error":"not_found"}"#)
}
fn http_500() -> String {
    "HTTP/1.1 500\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
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

/// Create a shell-script artifact, store in ContentStore, return digest.
pub fn create_script_artifact(artifact_root: &PathBuf, script: &str) -> String {
    let dir = std::env::temp_dir().join(format!("ch_script_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("artifact.sh");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    store_artifact(artifact_root, &path)
}
