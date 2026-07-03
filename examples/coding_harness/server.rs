//! HTTP server with proper Content-Length parsing and bounded body handling.

use agent_core_kernel::harness::coding::config::CodingConfig;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

const MAX_BODY: usize = 2_200_000;

pub fn serve(listener: TcpListener, config: Arc<CodingConfig>) {
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let c = Arc::clone(&config);
                std::thread::spawn(move || handle(s, &c));
            }
            Err(e) => eprintln!("coding_harness accept: {e}"),
        }
    }
}

fn handle(mut stream: TcpStream, config: &CodingConfig) {
    // Read headers until \r\n\r\n
    let mut buf = Vec::with_capacity(8192);
    let header_end = loop {
        let mut chunk = [0u8; 1024];
        let n = match stream.read(&mut chunk) {
            Ok(0) => {
                let _ = respond(&mut stream, 400, "connection_closed");
                return;
            }
            Ok(n) => n,
            Err(_) => {
                let _ = respond(&mut stream, 400, "read_error");
                return;
            }
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 65536 {
            let _ = respond(&mut stream, 413, "headers_too_large");
            return;
        }
    };

    // Parse Content-Length
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = match parse_cl(&headers) {
        Ok(Some(n)) => n,
        Ok(None) => {
            let _ = respond(&mut stream, 400, "missing_content_length");
            return;
        }
        Err(e) => {
            let _ = respond(&mut stream, 400, e);
            return;
        }
    };

    if content_length > MAX_BODY {
        let _ = respond(&mut stream, 413, "body_too_large");
        return;
    }
    if has_chunked(&headers) {
        let _ = respond(&mut stream, 400, "chunked_not_supported");
        return;
    }

    // Read body
    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; (content_length - body.len()).min(65536)];
        let n = match stream.read(&mut chunk) {
            Ok(0) => {
                let _ = respond(&mut stream, 400, "body_truncated");
                return;
            }
            Ok(n) => n,
            Err(_) => {
                let _ = respond(&mut stream, 400, "body_read_error");
                return;
            }
        };
        body.extend_from_slice(&chunk[..n]);
    }

    let body_str = match String::from_utf8(body) {
        Ok(s) => s,
        Err(_) => {
            let _ = respond(&mut stream, 400, "invalid_utf8");
            return;
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(v) => v,
        Err(_) => {
            let _ = respond(&mut stream, 400, "invalid_json");
            return;
        }
    };

    let pv = parsed
        .get("protocol_version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if pv != "external-harness-v1" {
        let _ = respond(&mut stream, 400, "unsupported_protocol");
        return;
    }

    let op = parsed
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args = parsed
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let resp_body = crate::protocol::dispatch(config, op, &args);
    let body_str = serde_json::to_string(&resp_body).unwrap_or_default();
    let _ = respond(&mut stream, 200, &body_str);
}

fn parse_cl(headers: &str) -> Result<Option<usize>, &'static str> {
    let mut found: Option<usize> = None;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                let trimmed = value.trim();
                if trimmed.is_empty() || trimmed.starts_with('+') || trimmed.starts_with('-') {
                    return Err("invalid_content_length");
                }
                let n: usize = trimmed.parse().map_err(|_| "invalid_content_length")?;
                match found {
                    Some(p) if p != n => return Err("conflicting_content_length"),
                    _ => found = Some(n),
                }
            }
        }
    }
    Ok(found)
}

fn has_chunked(headers: &str) -> bool {
    headers
        .lines()
        .filter_map(|l| l.split_once(':'))
        .any(|(n, v)| {
            n.eq_ignore_ascii_case("transfer-encoding") && v.trim().eq_ignore_ascii_case("chunked")
        })
}

fn respond(stream: &mut TcpStream, status: u16, error_code: &str) -> std::io::Result<()> {
    let body = format!(
        r#"{{"protocol_version":"external-harness-v1","ok":false,"error_code":"{error_code}"}}"#
    );
    let reason = if status == 200 { "OK" } else { "Error" };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    stream.write_all(resp.as_bytes())
}
