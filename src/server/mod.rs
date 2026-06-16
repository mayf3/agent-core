use crate::config::KernelConfig;
use crate::domain::OutboxDispatchStatus;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
mod delivery;
mod dispatcher_metrics;

use anyhow::{bail, Result};
use delivery::{recover_undelivered_ingress, start_outbox_dispatcher_loop, start_worker_loop};
pub use dispatcher_metrics::DispatcherMetrics;
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

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
    install_shutdown_handler(&running)?;
    let journal = Arc::new(JournalStore::open(&config.db_path)?);
    let recovered = journal.recover_unknown_invocations()?;
    if recovered > 0 {
        println!("agent-core recovered {recovered} unknown invocation(s)");
    }
    let gateway = Arc::new(Gateway::new(config.clone()));
    let recovered_ingress = recover_undelivered_ingress(Arc::clone(&journal))?;
    if recovered_ingress > 0 {
        println!("agent-core queued {recovered_ingress} undelivered ingress event(s)");
    }
    log_dispatcher_startup_state(&journal, config.outbox_dispatcher_enabled)?;
    let dispatcher_metrics = Arc::new(DispatcherMetrics::new());
    let worker = start_worker_loop(
        config.clone(),
        Arc::clone(&journal),
        Arc::clone(&gateway),
        Arc::clone(&running),
    );
    let outbox_dispatcher = start_outbox_dispatcher_loop(
        config.clone(),
        Arc::clone(&journal),
        Arc::clone(&running),
        Arc::clone(&dispatcher_metrics),
    );
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Err(error) = handle_connection(
                    &mut stream,
                    &config,
                    Arc::clone(&journal),
                    Arc::clone(&gateway),
                    Arc::clone(&dispatcher_metrics),
                ) {
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
            Err(error) => eprintln!("kernel accept failed: {error}"),
        }
    }
    if worker.join().is_err() {
        eprintln!("kernel worker thread panicked");
    }
    if outbox_dispatcher.join().is_err() {
        eprintln!("kernel outbox dispatcher thread panicked");
    }
    println!("agent-core kernel stopped gracefully");
    Ok(())
}

fn log_dispatcher_startup_state(journal: &JournalStore, enabled: bool) -> Result<()> {
    let pending = journal.outbox_status_count(OutboxDispatchStatus::Pending)?;
    let unknown = journal.outbox_status_count(OutboxDispatchStatus::Unknown)?;
    let dispatching = journal.outbox_status_count(OutboxDispatchStatus::Dispatching)?;
    println!("outbox_dispatcher_enabled={enabled}");
    println!("existing_pending_outbox_count={pending}");
    println!("existing_unknown_outbox_count={unknown}");
    println!("existing_dispatching_outbox_count={dispatching}");
    println!("dispatcher will process pending/retryable outbox items");
    println!("unknown items will not be retried automatically");
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
    dispatcher_metrics: Arc<DispatcherMetrics>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let request = read_request(stream)?;
    if request.method == "GET" && request.path == "/health" {
        return write_json(
            stream,
            200,
            health_snapshot(
                &journal,
                config.outbox_dispatcher_enabled,
                &dispatcher_metrics,
            )?,
        );
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

pub fn health_snapshot(
    journal: &JournalStore,
    outbox_dispatcher_enabled: bool,
    dispatcher_metrics: &DispatcherMetrics,
) -> Result<Value> {
    let hash_chain_ok = journal.verify_hash_chain()?;
    let unknown_invocations = journal.unknown_invocations()?;
    let undelivered_ingress_count = journal.undelivered_ingress_events()?.len();
    let worker_job_counts = journal.worker_job_status_counts()?;
    let outbox_dispatch_counts = journal.outbox_dispatch_status_counts()?;
    let outbox_pending_count = journal.outbox_status_count(OutboxDispatchStatus::Pending)?;
    let outbox_unknown_count = journal.outbox_status_count(OutboxDispatchStatus::Unknown)?;
    let outbox_dispatching_count =
        journal.outbox_status_count(OutboxDispatchStatus::Dispatching)?;
    let outbox_stale_dispatching_count = journal.outbox_stale_dispatching_count()?;
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
        "worker_jobs": worker_job_counts,
        "outbox_dispatches": outbox_dispatch_counts,
        "outbox_dispatcher_enabled": outbox_dispatcher_enabled,
        "outbox_dispatcher_running": dispatcher_metrics.is_running(),
        "last_dispatch_tick_at": dispatcher_metrics.last_tick_at(),
        "last_dispatch_error_category": dispatcher_metrics.last_error_category(),
        "outbox_pending_count": outbox_pending_count,
        "outbox_unknown_count": outbox_unknown_count,
        "outbox_dispatching_count": outbox_dispatching_count,
        "outbox_stale_dispatching_count": outbox_stale_dispatching_count,
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
