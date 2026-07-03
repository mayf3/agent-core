//! Simple localhost HTTP server for the workspace harness.
//!
//! Accepts connections, parses HTTP requests, dispatches to protocol handler,
//! and writes HTTP responses. Same pattern as `examples/time_harness.rs`.

use crate::config::WorkspaceConfig;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// Maximum request body bytes we accept.
const MAX_BODY_BYTES: usize = 262_144; // 256 KiB

/// Run the harness server loop. Returns when the listener stops accepting.
pub fn serve(listener: TcpListener, config: Arc<WorkspaceConfig>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let config = Arc::clone(&config);
                std::thread::spawn(move || handle_client(stream, &config));
            }
            Err(e) => {
                eprintln!("workspace_harness accept error: {e}");
            }
        }
    }
}

fn handle_client(mut stream: TcpStream, config: &WorkspaceConfig) {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);
    let body = match extract_body(&request) {
        Some(b) => b,
        None => {
            let _ = respond(&mut stream, 400, r#"{"error":"malformed_request"}"#);
            return;
        }
    };

    if body.len() > MAX_BODY_BYTES {
        let _ = respond(
            &mut stream,
            413,
            r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"body_too_large"}"#,
        );
        return;
    }

    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            let _ = respond(
                &mut stream,
                400,
                r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"invalid_json"}"#,
            );
            return;
        }
    };

    // Check protocol_version.
    let protocol_version = parsed
        .get("protocol_version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if protocol_version != "external-harness-v1" {
        let _ = respond(
            &mut stream,
            400,
            r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"unsupported_protocol"}"#,
        );
        return;
    }

    let operation = parsed
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let args = parsed
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let response_body = crate::protocol::dispatch(config, operation, &args);
    let body_str = serde_json::to_string(&response_body).unwrap_or_default();
    let _ = respond(&mut stream, 200, &body_str);
}

fn extract_body(request: &str) -> Option<&str> {
    request.split("\r\n\r\n").nth(1)
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())
}
