//! Calculator Harness — standalone external harness for basic arithmetic.
//!
//! Operations: external.calculator
//!   - add(a, b), subtract(a, b), multiply(a, b), divide(a, b)
//!
//! Protocol: external-harness-v1 (HTTP JSON).
//! Usage: cargo run --example calculator -- --listen 127.0.0.1:7300

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let addr = args
        .iter()
        .position(|a| a == "--listen")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:7300".to_string());

    let listener = TcpListener::bind(&addr).expect("failed to bind");
    eprintln!("calculator_harness listening on {addr}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(|| handle(stream));
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

fn handle(mut stream: TcpStream) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let body = match request.split("\r\n\r\n").nth(1) {
        Some(b) => b,
        None => {
            let _ = respond(
                &mut stream,
                400,
                r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"malformed"}"#,
            );
            return;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            respond(
                &mut stream,
                400,
                r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"invalid_json"}"#,
            );
            return;
        }
    };

    if parsed.get("protocol_version").and_then(|v| v.as_str()) != Some("external-harness-v1") {
        respond(
            &mut stream,
            400,
            r#"{"protocol_version":"external-harness-v1","ok":false,"error_code":"unsupported_protocol"}"#,
        );
        return;
    }

    // Calculator expects: { operation: "add"|"subtract"|"multiply"|"divide", a: number, b: number }
    let op = parsed
        .get("arguments")
        .and_then(|a| a.get("operation"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let a = parsed
        .get("arguments")
        .and_then(|a| a.get("a"))
        .and_then(|v| v.as_f64())
        .unwrap_or(f64::NAN);
    let b = parsed
        .get("arguments")
        .and_then(|a| a.get("b"))
        .and_then(|v| v.as_f64())
        .unwrap_or(f64::NAN);

    let result = match op {
        "add" => Ok(a + b),
        "subtract" => Ok(a - b),
        "multiply" => Ok(a * b),
        "divide" => {
            if b == 0.0 {
                Err("divide_by_zero")
            } else {
                Ok(a / b)
            }
        }
        _ => Err("unsupported_operation"),
    };

    match result {
        Ok(val) => {
            let resp = serde_json::json!({
                "protocol_version": "external-harness-v1",
                "ok": true,
                "result": { "result": val }
            });
            respond(
                &mut stream,
                200,
                &serde_json::to_string(&resp).unwrap_or_default(),
            );
        }
        Err(code) => {
            let resp = serde_json::json!({
                "protocol_version": "external-harness-v1",
                "ok": false,
                "error_code": code,
            });
            respond(
                &mut stream,
                200,
                &serde_json::to_string(&resp).unwrap_or_default(),
            );
        }
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    let _ = stream.write_all(response.as_bytes());
}
