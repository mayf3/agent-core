fn main() {
    // context-test-compactor — always returns passthrough ContextPlan.
    // Used to prove Provider replaceability without Kernel changes.
    eprintln!("test-compactor starting on 127.0.0.1:7203");
    let listener = std::net::TcpListener::bind("127.0.0.1:7203").expect("bind");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => std::thread::spawn(|| handle(s)),
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

fn handle(mut stream: std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                if buf.len() > 65536 { return; }
            }
            Err(_) => return,
        }
    }
    let s = String::from_utf8_lossy(&buf);
    let body_start = s.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    let body = &s[body_start..];

    let Ok(req) = serde_json::from_str::<serde_json::Value>(body) else {
        let _ = respond(&mut stream, 400, r#"{"hook":"context.compress.v0","ok":false}"#);
        return;
    };

    let payload = req.get("payload");
    let items = payload.and_then(|p| p.get("candidate_context_items")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let through = payload.and_then(|p| p.get("through_event_id")).and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Passthrough: keep everything
    let plan_items: Vec<serde_json::Value> = items.iter().enumerate().map(|(i, _)| {
        serde_json::json!({"index": i, "action": "keep"})
    }).collect();

    let plan_digest = {
        let mut h = sha2::Sha256::new();
        for pi in &plan_items {
            h.update(serde_json::to_string(pi).unwrap_or_default().as_bytes());
        }
        hex::encode(h.finalize())
    };

    let resp = serde_json::json!({
        "hook": "context.compress.v0",
        "ok": true,
        "payload": {
            "provider_id": "test-compactor-v0",
            "through_event_id": through,
            "mode": "passthrough",
            "context_items": plan_items,
            "estimated_size": 0,
            "plan_digest": plan_digest,
        }
    });

    let body = serde_json::to_string(&resp).unwrap_or_default();
    let _ = respond(&mut stream, 200, &body);
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    write!(stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    )
}

use std::io::Read;