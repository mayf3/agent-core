fn main() {
    // context-simple-compactor v0 — external Context Provider
    // Listens for context.compress.v0 requests, returns ContextPlan.
    let port: u16 = std::env::var("SIMPLECOMPACTOR_PORT")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(7202);
    eprintln!("simple-compactor starting on 127.0.0.1:{port}");
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => { std::thread::spawn(|| handle(s)); },
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
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 65536 {
                    let _ = respond(&mut stream, 413, r#"{"hook":"context.compress.v0","ok":false}"#);
                    return;
                }
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
    let items = payload
        .and_then(|p| p.get("candidate_context_items"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let budget = payload
        .and_then(|p| p.get("model_context_budget"))
        .and_then(|v| v.as_u64())
        .unwrap_or(4096) as usize;

    // Deterministic compression: keep items under budget, truncate large tool results
    let mut estimated = 0usize;
    let mut plan_items = Vec::new();
    let mut content_store: Vec<String> = Vec::new();

    for (i, item) in items.iter().enumerate() {
        let text = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");

        // System/required items are always kept
        let is_required = kind == "RootSystem" || kind == "UserMessage" || kind == "ToolCatalog";

        if is_required {
            estimated += text.len();
            plan_items.push(serde_json::json!({"index": i, "action": "keep"}));
            continue;
        }

        let is_tool_result = kind == "ToolResult";
        let max_preview: usize = 4096;

        if is_tool_result && text.len() > max_preview {
            let original_bytes = text.len();
            let digest = {
                let mut h = sha2::Sha256::default();
                h.update(text.as_bytes());
                hex::encode(h.finalize())
            };
            let preview = utf8_truncate(text, max_preview);
            let truncated = format!("{}...\n[truncated=true original_bytes={} result_digest={}]", preview, original_bytes, digest);
            estimated += truncated.len();
            content_store.push(truncated);
            plan_items.push(serde_json::json!({
                "index": i,
                "action": "truncate",
                "content": &content_store[content_store.len()-1],
                "original_bytes": original_bytes,
                "digest": digest,
            }));
        } else if estimated + text.len() <= budget {
            estimated += text.len();
            plan_items.push(serde_json::json!({"index": i, "action": "keep"}));
        } else {
            let original_bytes = text.len();
            let digest = {
                let mut h = sha2::Sha256::default();
                h.update(text.as_bytes());
                hex::encode(h.finalize())
            };
            plan_items.push(serde_json::json!({
                "index": i,
                "action": "drop",
                "original_bytes": original_bytes,
                "digest": digest,
            }));
        }
    }

    let plan_digest = {
        let mut h = sha2::Sha256::default();
        for pi in &plan_items {
            h.update(serde_json::to_string(pi).unwrap_or_default().as_bytes());
        }
        hex::encode(h.finalize())
    };

    let resp = serde_json::json!({
        "hook": "context.compress.v0",
        "ok": true,
        "payload": {
            "provider_id": "simple-compactor-v0",
            "through_event_id": payload.and_then(|p| p.get("through_event_id")).and_then(|v| v.as_str()).unwrap_or(""),
            "mode": if items.len() > 20 { "compacted" } else { "passthrough" },
            "context_items": plan_items,
            "estimated_size": estimated,
            "plan_digest": plan_digest,
        }
    });

    let body = serde_json::to_string(&resp).unwrap_or_default();
    let _ = respond(&mut stream, 200, &body);
}

fn utf8_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut bound = max_bytes.min(s.len());
    while bound > 0 && !s.is_char_boundary(bound) {
        bound -= 1;
    }
    &s[..bound]
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    write!(stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    )
}

use std::io::Write;
use std::io::Read;
use sha2::Digest;