//! Minimal loopback HTTP server implementing the external harness v1
//! protocol as consumed by the Kernel adapter (`external_harness.rs`).

use crate::exec;
use crate::workspace;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;

/// Hard cap on request body size (defends the listener against oversized
/// bodies; the Kernel adapter itself caps responses at 64 KiB).
const MAX_REQUEST_BYTES: usize = 256 * 1024;
/// Maximum concurrent connection threads.
const MAX_THREADS: usize = 16;

pub struct Config {
    pub token: String,
    pub workspace_root: std::path::PathBuf,
}

pub fn serve(listener: TcpListener, config: Config) -> ExitCode {
    let config = Arc::new(config);
    let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cfg = Arc::clone(&config);
                handles.push(std::thread::spawn(move || {
                    let _ = handle_connection(stream, cfg);
                }));
                // Keep a bounded worker set: drop finished threads and block
                // on the oldest still-running one when the cap is reached.
                handles.retain(|h| !h.is_finished());
                while handles.len() >= MAX_THREADS {
                    let oldest = handles.remove(0);
                    let _ = oldest.join();
                }
            }
            Err(_) => continue,
        }
    }
    ExitCode::SUCCESS
}

fn handle_connection(mut stream: TcpStream, config: Arc<Config>) -> std::io::Result<()> {
    // Read the request head (bounded).
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end: Option<usize> = None;
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = Some(pos + 4);
            break;
        }
        if buf.len() > MAX_REQUEST_BYTES {
            return respond(&mut stream, 413, "request_too_large");
        }
    }
    let Some(header_end) = header_end else {
        return respond(&mut stream, 400, "malformed_request");
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    if path != "/execute" {
        return respond(&mut stream, 404, "not_found");
    }

    // Parse Content-Length (default 0).
    let mut content_length = 0usize;
    let mut bearer = None;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = lower.strip_prefix("authorization:") {
            let v = v.trim();
            if let Some(token) = v.strip_prefix("bearer ") {
                bearer = Some(token.trim().to_string());
            }
        }
    }
    if content_length > MAX_REQUEST_BYTES {
        return respond(&mut stream, 413, "request_too_large");
    }
    let body_start = header_end;
    let body_end = body_start + content_length;
    if buf.len() < body_end {
        // Read remaining body bytes.
        let mut rest = vec![0u8; body_end - buf.len()];
        stream.read_exact(&mut rest)?;
        buf.extend_from_slice(&rest);
    }
    let body: Value = match serde_json::from_slice(&buf[body_start..body_end]) {
        Ok(v) => v,
        Err(_) => return respond(&mut stream, 400, "invalid_json"),
    };

    // Authentication: fail closed.
    let expected = config.token.as_str();
    match bearer {
        Some(b) if b == expected => {}
        _ => return respond(&mut stream, 401, "unauthorized"),
    }

    // Protocol envelope validation (mirrors the Kernel adapter contract).
    if body.get("protocol_version").and_then(Value::as_str) != Some("external-harness-v1") {
        return respond(&mut stream, 400, "unsupported_protocol");
    }
    let operation = body
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = body.get("arguments").cloned().unwrap_or(json!({}));

    let result = dispatch(&config.workspace_root, operation, &arguments);
    match result {
        Ok(outcome) => respond_json(
            &mut stream,
            200,
            &json!({
                "protocol_version": "external-harness-v1",
                "ok": true,
                "result": outcome,
            }),
        ),
        Err(rejection) => respond_json(
            &mut stream,
            200,
            &json!({
                "protocol_version": "external-harness-v1",
                "ok": false,
                "error_code": rejection,
            }),
        ),
    }
}

fn dispatch(root: &std::path::Path, operation: &str, arguments: &Value) -> Result<Value, String> {
    match operation {
        "external.coding_workspace_list" => workspace::list(root, arguments),
        "external.coding_workspace_read" => workspace::read(root, arguments),
        "external.coding_workspace_write" => workspace::write(root, arguments),
        "external.coding_workspace_exec" => exec::execute(root, arguments),
        _ => Err("unknown_operation".to_string()),
    }
}

fn respond(stream: &mut TcpStream, status: u16, error_code: &str) -> std::io::Result<()> {
    let body = json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": error_code,
    });
    respond_json(stream, status, &body)
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let reason = if body.get("ok").and_then(Value::as_bool) == Some(true) {
        "OK"
    } else {
        "Error"
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&bytes)
}
