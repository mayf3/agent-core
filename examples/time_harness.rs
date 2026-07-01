//! External time harness example — a standalone localhost process.
//!
//! Usage:
//!   cargo run --example time_harness -- --listen 127.0.0.1:7101
//!
//! This is an independent process fixture. It does NOT call any Kernel
//! internal API, read .env files, or access any database.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

const PROTOCOL_VERSION: &str = "external-harness-v1";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let listen_addr = if let Some(idx) = args.iter().position(|a| a == "--listen") {
        args.get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "127.0.0.1:7101".to_string())
    } else {
        "127.0.0.1:7101".to_string()
    };

    let listener = TcpListener::bind(&listen_addr).expect("failed to bind");
    eprintln!("time_harness listening on {listen_addr}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(|| handle_client(stream));
            }
            Err(e) => {
                eprintln!("accept error: {e}");
            }
        }
    }
}

fn handle_client(mut stream: TcpStream) {
    let mut buf = [0u8; 4096];
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

    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            let _ = respond(&mut stream, 400, r#"{"error":"invalid_json"}"#);
            return;
        }
    };

    // Check protocol_version.
    if parsed.get("protocol_version").and_then(|v| v.as_str()) != Some(PROTOCOL_VERSION) {
        let _ = respond(
            &mut stream,
            400,
            r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"unsupported_protocol"}"#,
        );
        return;
    }

    // Check operation.
    let operation = parsed
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match operation {
        "external.time_now" => handle_time_now(&mut stream),
        _ => {
            let _ = respond(
                &mut stream,
                404,
                r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"unknown_operation"}"#,
            );
        }
    }
}

fn handle_time_now(stream: &mut TcpStream) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let epoch_ms = now.as_millis() as u64;

    // ISO 8601 format
    let iso = chrono::Utc::now().to_rfc3339();

    let response = serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "ok": true,
        "result": {
            "iso": iso,
            "epoch_ms": epoch_ms,
        }
    });

    let _ = respond(stream, 200, &response.to_string());
}

fn extract_body(request: &str) -> Option<&str> {
    let parts: Vec<&str> = request.split("\r\n\r\n").collect();
    if parts.len() >= 2 {
        Some(parts[1])
    } else {
        None
    }
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
