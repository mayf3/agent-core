//! Minimal loopback HTTP server for the Development Controller.
//!
//! Speaks exactly one route: `POST /v1/orchestrations`. It accepts a JSON
//! [`ExternalOrchestrationIntent`] body and returns a JSON
//! [`ExternalOrchestrationResult`].
//!
//! The server binds to loopback only. It carries no secrets beyond what the
//! Kernel already trusts on the loopback boundary; authentication of the
//! *principal* is the Kernel's job, not the Controller's.

use crate::{handle_intent, ControllerConfig};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

/// Run the controller server until the process is interrupted (SIGINT / ctrlc).
pub fn serve(config: ControllerConfig) -> Result<()> {
    let listener = TcpListener::bind(&config.bind_addr)?;
    listener.set_nonblocking(true)?;
    println!(
        "development-controller listening on {} (POST /v1/orchestrations)",
        config.bind_addr
    );
    let running = Arc::new(AtomicBool::new(true));
    install_shutdown_handler(Arc::clone(&running))?;
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Err(error) = handle_connection(&mut stream) {
                    let _ = write_json(
                        &mut stream,
                        500,
                        json!({ "ok": false, "error": error.to_string() }),
                    );
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => eprintln!("controller accept failed: {error}"),
        }
    }
    println!("development-controller stopped gracefully");
    Ok(())
}

fn handle_connection(stream: &mut std::net::TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let request = read_request(stream)?;
    if request.method != "POST" || request.path != "/v1/orchestrations" {
        return write_json(
            stream,
            404,
            json!({ "ok": false, "error": "not_found", "route": "POST /v1/orchestrations" }),
        );
    }
    let intent: agent_core_protocol::ExternalOrchestrationIntent =
        serde_json::from_slice(&request.body)
            .map_err(|e| anyhow::anyhow!("invalid_intent: {e}"))?;
    match handle_intent(&intent) {
        Ok(result) => write_json(stream, 200, json!({ "ok": true, "result": result })),
        Err(e) => write_json(stream, 400, json!({ "ok": false, "error": e.to_string() })),
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut std::net::TcpStream) -> Result<HttpRequest> {
    let mut buffer = Vec::new();
    let mut temp = [0u8; 1024];
    loop {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(header_end) = find_header_end(&buffer) {
            let head = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = content_length(&head);
            let total = header_end + 4 + content_length;
            while buffer.len() < total {
                let read = stream.read(&mut temp)?;
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp[..read]);
            }
            return parse_request(&buffer[..total]);
        }
    }
    bail!("empty request")
}

fn parse_request(buffer: &[u8]) -> Result<HttpRequest> {
    let header_end =
        find_header_end(buffer).ok_or_else(|| anyhow::anyhow!("missing HTTP headers"))?;
    let head = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = head.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    Ok(HttpRequest {
        method,
        path,
        body: buffer[header_end + 4..].to_vec(),
    })
}

fn write_json(stream: &mut std::net::TcpStream, status: u16, body: Value) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let payload = serde_json::to_string(&body)?;
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

fn install_shutdown_handler(running: Arc<AtomicBool>) -> Result<()> {
    ctrlc::set_handler(move || {
        running.store(false, Ordering::SeqCst);
    })
    .map_err(|e| anyhow::anyhow!("failed to install shutdown handler: {e}"))
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}
