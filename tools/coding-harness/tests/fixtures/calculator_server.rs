//! Calculator External Harness — standalone HTTP server.
//!
//! Uses only stdlib (no external dependencies).
//! Implements "external-harness-v1" for "external.calculator".
//!
//! Usage: CALC_PORT=<port> ./calculator_server
//!   or: CALC_PORT=<port> cargo run --manifest-path <path>/Cargo.toml

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

fn main() {
    let port = std::env::var("CALC_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(7300);

    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind failed");
    eprintln!("calculator_harness listening on {port}");

    for stream in listener.incoming() {
        match stream {
            Ok(s) => { std::thread::spawn(|| handle(s)); }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

fn handle(mut stream: TcpStream) {
    let mut buf = [0u8; 65536];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let body = match request.split("\r\n\r\n").nth(1) {
        Some(b) => b,
        None => return respond(&mut stream, 400, r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"malformed"}"#),
    };

    // Minimal JSON parsing without serde.
    // Extract protocol_version.
    if !body.contains(r#""external-harness-v1""#) {
        return respond(&mut stream, 400, r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"unsupported_protocol"}"#);
    }

    // Extract operation.
    let op = extract_string(body, "\"operation\"");
    // Extract a and b.
    let a = extract_number(body, "\"a\"");
    let b = extract_number(body, "\"b\"");

    let resp = match op.as_deref() {
        Some("add") => ok_json(a + b),
        Some("subtract") => ok_json(a - b),
        Some("multiply") => ok_json(a * b),
        Some("divide") => {
            if b == 0.0 {
                err_json("divide_by_zero")
            } else {
                ok_json(a / b)
            }
        }
        _ => err_json("unsupported_operation"),
    };
    respond(&mut stream, 200, &resp);
}

fn ok_json(val: f64) -> String {
    format!(r#"{{"protocol_version":"external-harness-v1","ok":true,"result":{{"result":{val}}}}}"#)
}

fn err_json(code: &str) -> String {
    format!(r#"{{"protocol_version":"external-harness-v1","ok":false,"error_code":"{code}"}}"#)
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// Extract a string value from JSON by key (naive but sufficient for this protocol).
fn extract_string(body: &str, key: &str) -> Option<String> {
    let search = format!(r#"{key}:"#);
    if let Some(pos) = body.find(&search) {
        let rest = &body[pos + search.len()..];
        let rest = rest.trim_start().trim_start_matches('"');
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Extract a f64 number from JSON by key.
fn extract_number(body: &str, key: &str) -> f64 {
    let search = format!(r#"{key}:"#);
    if let Some(pos) = body.find(&search) {
        let rest = &body[pos + search.len()..];
        let rest = rest.trim_start();
        let end = rest.find(|c: char| !"0123456789.eE+-".contains(c)).unwrap_or(rest.len());
        rest[..end].parse().unwrap_or(0.0)
    } else {
        0.0
    }
}
