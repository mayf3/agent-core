use crate::adapters::HttpConnectorAdapter;
use crate::config::KernelConfig;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::OpenAiCompatibleLlm;
use crate::runtime::Runtime;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

type DeliveryThreads = Arc<Mutex<Vec<thread::JoinHandle<()>>>>;

pub fn serve(config: KernelConfig) -> Result<()> {
    if config.ipc_token.is_empty() {
        bail!("AGENT_CORE_IPC_TOKEN is required for serve");
    }
    let listener = TcpListener::bind(("127.0.0.1", config.kernel_port))?;
    println!(
        "agent-core kernel listening on 127.0.0.1:{}",
        config.kernel_port
    );
    listener.set_nonblocking(true)?;
    let running = Arc::new(AtomicBool::new(true));
    let deliveries = Arc::new(Mutex::new(Vec::new()));
    install_shutdown_handler(&running)?;
    let journal = Arc::new(JournalStore::open(&config.db_path)?);
    let recovered = journal.recover_unknown_invocations()?;
    if recovered > 0 {
        println!("agent-core recovered {recovered} unknown invocation(s)");
    }
    let gateway = Arc::new(Gateway::new(config.clone()));
    let recovered_ingress = recover_undelivered_ingress(
        config.clone(),
        Arc::clone(&journal),
        Arc::clone(&gateway),
        Arc::clone(&deliveries),
    )?;
    if recovered_ingress > 0 {
        println!("agent-core queued {recovered_ingress} undelivered ingress event(s)");
    }
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Err(error) = handle_connection(
                    &mut stream,
                    &config,
                    Arc::clone(&journal),
                    Arc::clone(&gateway),
                    Arc::clone(&deliveries),
                ) {
                    let _ = write_json(
                        &mut stream,
                        500,
                        json!({ "ok": false, "error": error.to_string() }),
                    );
                }
                prune_finished_deliveries(&deliveries);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => eprintln!("kernel accept failed: {error}"),
        }
    }
    drain_delivery_threads(&deliveries);
    println!("agent-core kernel stopped gracefully");
    Ok(())
}

fn install_shutdown_handler(running: &Arc<AtomicBool>) -> Result<()> {
    let signal = Arc::clone(running);
    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })
    .map_err(|error| anyhow::anyhow!("failed to install shutdown handler: {error}"))
}

fn handle_connection(
    stream: &mut TcpStream,
    config: &KernelConfig,
    journal: Arc<JournalStore>,
    gateway: Arc<Gateway>,
    deliveries: DeliveryThreads,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let request = read_request(stream)?;
    if request.method == "GET" && request.path == "/health" {
        return write_json(stream, 200, health_snapshot(&journal)?);
    }
    if request.method != "POST" || request.path != "/v1/ingress" {
        return write_json(stream, 404, json!({ "ok": false, "error": "not_found" }));
    }
    if request.bearer_token.as_deref() != Some(config.ipc_token.as_str()) {
        return write_json(stream, 401, json!({ "ok": false, "error": "unauthorized" }));
    }
    let body: Value = serde_json::from_slice(&request.body)?;
    let envelope = serde_json::from_value(json!({
        "protocol_version": body.get("protocol_version").cloned().unwrap_or_else(|| json!("")),
        "source": body.get("source").cloned().unwrap_or_else(|| json!("")),
        "external_event_id": body.get("external_event_id").cloned().unwrap_or_else(|| json!("")),
        "received_at": body.get("received_at").cloned().unwrap_or_else(|| json!("")),
        "payload": body.get("payload").cloned().unwrap_or_else(|| json!({})),
        "auth_context": { "authenticated": true },
        "routing_hint": body.get("routing_hint").cloned(),
    }))?;
    let validated = match gateway.validate_ingress(&journal, envelope) {
        Ok(event) => event,
        Err(error) if error.to_string().starts_with("skip:") => {
            return write_json(stream, 200, json!({ "ok": true, "status": "skipped" }));
        }
        Err(error) => {
            return write_json(
                stream,
                400,
                json!({ "ok": false, "error": error.to_string() }),
            )
        }
    };
    let kernel_event_id = validated.event_id.0.clone();
    spawn_delivery(config.clone(), journal, gateway, validated, deliveries);
    write_json(
        stream,
        200,
        json!({
            "ok": true,
            "status": "accepted",
            "kernel_event_id": kernel_event_id,
        }),
    )
}

fn spawn_delivery(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    gateway: Arc<Gateway>,
    event: crate::domain::ValidatedEvent,
    deliveries: DeliveryThreads,
) {
    let event_id = event.event_id.0.clone();
    let handle = thread::spawn(move || {
        if let Err(error) = deliver_event(config, &journal, &gateway, event) {
            eprintln!(
                "kernel async delivery failed event={} category={}",
                short_id(&event_id),
                error_category(&error)
            );
            let _ = journal.append_event(
                crate::domain::JournalEventKind::RunCompleted,
                None,
                None,
                Some(&event_id),
                json!({
                    "status": "Failed",
                    "reason": "async_delivery_failed",
                    "error_category": error_category(&error),
                }),
            );
        }
    });
    match deliveries.lock() {
        Ok(mut handles) => handles.push(handle),
        Err(_) => {
            eprintln!("kernel delivery tracker unavailable; waiting for delivery inline");
            let _ = handle.join();
        }
    }
}

fn prune_finished_deliveries(deliveries: &DeliveryThreads) {
    let mut finished = Vec::new();
    if let Ok(mut handles) = deliveries.lock() {
        let mut pending = Vec::with_capacity(handles.len());
        for handle in handles.drain(..) {
            if handle.is_finished() {
                finished.push(handle);
            } else {
                pending.push(handle);
            }
        }
        *handles = pending;
    }
    for handle in finished {
        if handle.join().is_err() {
            eprintln!("kernel delivery thread panicked");
        }
    }
}

fn drain_delivery_threads(deliveries: &DeliveryThreads) {
    let handles = match deliveries.lock() {
        Ok(mut handles) => handles.drain(..).collect::<Vec<_>>(),
        Err(_) => {
            eprintln!("kernel delivery tracker unavailable during shutdown");
            return;
        }
    };
    if !handles.is_empty() {
        println!("agent-core draining {} delivery thread(s)", handles.len());
    }
    for handle in handles {
        if handle.join().is_err() {
            eprintln!("kernel delivery thread panicked");
        }
    }
}

fn recover_undelivered_ingress(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    gateway: Arc<Gateway>,
    deliveries: DeliveryThreads,
) -> Result<usize> {
    let events = journal.undelivered_ingress_events()?;
    let mut recovered = 0;
    for event in events {
        match gateway.recover_validated_event(&event) {
            Ok(validated) => {
                recovered += 1;
                spawn_delivery(
                    config.clone(),
                    Arc::clone(&journal),
                    Arc::clone(&gateway),
                    validated,
                    Arc::clone(&deliveries),
                );
            }
            Err(error) => {
                journal.append_event(
                    crate::domain::JournalEventKind::RunCompleted,
                    None,
                    None,
                    event.payload.get("event_id").and_then(Value::as_str),
                    json!({
                        "status": "Failed",
                        "reason": "undelivered_ingress_recovery_failed",
                        "error_category": error_category(&error),
                    }),
                )?;
            }
        }
    }
    Ok(recovered)
}

fn deliver_event(
    config: KernelConfig,
    journal: &JournalStore,
    gateway: &Gateway,
    validated: crate::domain::ValidatedEvent,
) -> Result<()> {
    let adapter = HttpConnectorAdapter::new(
        config.connector_execute_url.clone(),
        config.ipc_token.clone(),
    );
    let llm = OpenAiCompatibleLlm::new(
        config.openai_base_url.clone(),
        config.openai_api_key.clone(),
        config.model.clone(),
        config.model_timeout_ms,
    )
    .with_fallback(
        config.fallback_openai_base_url.clone(),
        config.fallback_openai_api_key.clone(),
        config.fallback_model.clone(),
    );
    let llm = Box::new(llm);
    let runtime = Runtime::new(config.clone(), llm, adapter);
    runtime.deliver(journal, gateway, validated)?;
    Ok(())
}

pub fn health_snapshot(journal: &JournalStore) -> Result<Value> {
    let hash_chain_ok = journal.verify_hash_chain()?;
    let unknown_invocations = journal.unknown_invocations()?;
    let undelivered_ingress_count = journal.undelivered_ingress_events()?.len();
    let status = if !hash_chain_ok {
        "corrupt"
    } else if unknown_invocations.is_empty() {
        "ok"
    } else {
        "degraded"
    };
    Ok(json!({
        "ok": hash_chain_ok,
        "status": status,
        "hash_chain_ok": hash_chain_ok,
        "journal_event_count": journal.event_count()?,
        "undelivered_ingress_count": undelivered_ingress_count,
        "unknown_invocation_count": unknown_invocations.len(),
        "unknown_invocations": unknown_invocations.iter().map(|invocation| {
            json!({
                "invocation_id": invocation.invocation_id,
                "run_id": invocation.run_id.as_ref().map(|id| id.0.as_str()),
                "session_id": invocation.session_id.as_ref().map(|id| id.0.as_str()),
                "first_dispatch_at": invocation.first_dispatch_at.to_rfc3339(),
            })
        }).collect::<Vec<_>>(),
    }))
}

struct HttpRequest {
    method: String,
    path: String,
    bearer_token: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut buffer = Vec::new();
    let mut temp = [0_u8; 1024];
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
    let mut bearer_token = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") {
                bearer_token = value
                    .trim()
                    .strip_prefix("Bearer ")
                    .map(str::trim)
                    .map(str::to_string);
            }
        }
    }
    Ok(HttpRequest {
        method,
        path,
        bearer_token,
        body: buffer[header_end + 4..].to_vec(),
    })
}

fn write_json(stream: &mut TcpStream, status: u16, body: Value) -> Result<()> {
    let reason = if status == 200 {
        "OK"
    } else if status == 401 {
        "Unauthorized"
    } else if status == 404 {
        "Not Found"
    } else {
        "Error"
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

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(head: &str) -> usize {
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value.trim().parse().unwrap_or(0);
        }
    }
    0
}

fn error_category(error: &anyhow::Error) -> String {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("timeout") {
        "timeout".to_string()
    } else if message.contains("connector execute failed") {
        "connector_execute_failed".to_string()
    } else if message.contains("target_session") {
        "target_session_mismatch".to_string()
    } else {
        "runtime_failed".to_string()
    }
}

fn short_id(value: &str) -> String {
    if value.len() <= 10 {
        value.to_string()
    } else {
        format!("{}...{}", &value[..4], &value[value.len() - 4..])
    }
}
