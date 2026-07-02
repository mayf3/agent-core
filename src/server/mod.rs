use crate::config::KernelConfig;
use crate::domain::{OutboxDispatchStatus, RunId};
use crate::gateway::Gateway;
use crate::journal::JournalStore;
mod capability_http;
pub mod capability_routes;
mod delivery;
mod dispatcher_metrics;
pub mod harness_routes;
use anyhow::{bail, Result};
#[cfg(test)]
pub(crate) use delivery::build_llm_from_config;
use delivery::{
    recover_undelivered_ingress, start_approval_expiry_loop, start_outbox_dispatcher_loop,
    start_worker_loop,
};
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
    // Validate capability tokens: if configured, they must be distinct from
    // each other and from the legacy IPC token. Failure exits before any
    // listener starts — no HTTP request can observe a collision.
    validate_capability_tokens(&config)?;
    let listener = TcpListener::bind(("127.0.0.1", config.kernel_port))?;
    println!(
        "agent-core kernel listening on 127.0.0.1:{}",
        config.kernel_port
    );
    listener.set_nonblocking(true)?;
    let running = Arc::new(AtomicBool::new(true));
    install_shutdown_handler(&running)?;
    let journal = Arc::new(JournalStore::open(&config.db_path)?);
    // Initialize the registry (creates baseline snapshot, sets current,
    // backfills old Runs). This must succeed — without a registry, no Run
    journal.initialize_registry()?;
    let recovered = journal.recover_unknown_invocations()?;
    if recovered > 0 {
        println!("agent-core recovered {recovered} unknown invocation(s)");
    }
    // operator-configured TTL. No-op unless both require_write_approval and a
    if config.require_write_approval && config.write_approval_ttl_secs > 0 {
        let expired = journal.expire_stale_approvals(config.write_approval_ttl_secs)?;
        if expired > 0 {
            println!("agent-core expired {expired} stale approval(s)");
        }
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
    let approval_expiry =
        start_approval_expiry_loop(config.clone(), Arc::clone(&journal), Arc::clone(&running));
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
    if approval_expiry.join().is_err() {
        eprintln!("kernel approval-expiry thread panicked");
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
    // Non-health routes must be POST under /v1/.
    if request.method != "POST" || !request.path.starts_with("/v1/") {
        return write_json(stream, 404, json!({ "ok": false, "error": "not_found" }));
    }
    let path = request.path.as_str();
    let bearer = request.bearer_token.as_deref().unwrap_or("");

    // Try capability-specific routes first (extracted for structure gate).
    if capability_http::try_handle_capability_request(
        stream,
        path,
        &request.method,
        bearer,
        &request.body,
        config,
        &journal,
        &gateway,
    )? {
        return Ok(());
    }

    // ---- All other /v1/ routes require the IPC bearer token ----
    if bearer != config.ipc_token.as_str() {
        return write_json(stream, 401, json!({ "ok": false, "error": "unauthorized" }));
    } else if path == "/v1/ingress" {
        handle_ingress(stream, &gateway, &journal, &request)
    } else if path == "/v1/approve" {
        handle_approval_decision(stream, &gateway, &journal, &request, true)
    } else if path == "/v1/deny" {
        handle_approval_decision(stream, &gateway, &journal, &request, false)
    } else if path == "/v1/harness/register" {
        let body: Value = serde_json::from_slice(&request.body)?;
        handle_harness_result(
            stream,
            harness_routes::handle_register(&gateway, &journal, &body),
        )
    } else if path == "/v1/harness/enable" {
        let body: Value = serde_json::from_slice(&request.body)?;
        handle_harness_result(
            stream,
            harness_routes::handle_enable(&gateway, &journal, &body),
        )
    } else if path == "/v1/harness/disable" {
        let body: Value = serde_json::from_slice(&request.body)?;
        handle_harness_result(
            stream,
            harness_routes::handle_disable(&gateway, &journal, &body),
        )
    } else {
        write_json(stream, 404, json!({ "ok": false, "error": "not_found" }))
    }
}
fn handle_ingress(
    stream: &mut TcpStream,
    gateway: &Gateway,
    journal: &JournalStore,
    request: &HttpRequest,
) -> Result<()> {
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
    let validated = match gateway.validate_ingress(journal, envelope) {
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
/// Phase 2 M2d follow-up: handle `POST /v1/approve` (`approved == true`) and
/// `POST /v1/deny` (`approved == false`). Body: `{ "run_id": "<id>" }`. Both
/// delegate to the (idempotent) `Gateway::approve_run`/`deny_run`. A run that
/// is not `AwaitingApproval` is a no-op-200 (idempotent), matching the
fn handle_approval_decision(
    stream: &mut TcpStream,
    gateway: &Gateway,
    journal: &JournalStore,
    request: &HttpRequest,
    approved: bool,
) -> Result<()> {
    let body: Value = serde_json::from_slice(&request.body)?;
    let Some(run_id) = body.get("run_id").and_then(Value::as_str) else {
        return write_json(
            stream,
            400,
            json!({ "ok": false, "error": "missing run_id" }),
        );
    };
    let run_id = RunId(run_id.to_string());
    if approved {
        gateway.approve_run(journal, &run_id)?;
    } else {
        gateway.deny_run(journal, &run_id)?;
    }
    write_json(
        stream,
        200,
        json!({
            "ok": true,
            "run_id": run_id.0,
            "decision": if approved { "approved" } else { "denied" },
        }),
    )
}
/// Map harness route errors to appropriate HTTP status codes.
/// Never leaks database errors, paths, or tokens in the response body.
fn handle_harness_result(stream: &mut TcpStream, result: Result<String>) -> Result<()> {
    match result {
        Ok(body) => write_json(stream, 200, serde_json::from_str(&body)?),
        Err(e) => {
            // Prefer typed HarnessRouteError via downcast, fall back to
            // stable string matching for errors that still carry the old
            // prefix convention (e.g. manifest compute_manifest_id).
            let (status, safe_msg) =
                if let Some(hr_err) = e.downcast_ref::<harness_routes::HarnessRouteError>() {
                    (hr_err.http_status(), hr_err.safe_error())
                } else {
                    let msg = e.to_string();
                    if msg.starts_with("invalid_manifest") || msg.starts_with("invalid_request") {
                        (400, "invalid_request")
                    } else if msg.starts_with("unauthorized") {
                        (401, "unauthorized")
                    } else if msg.starts_with("manifest_not_found") {
                        (404, "not_found")
                    } else if msg.starts_with("snapshot_conflict")
                        || msg.starts_with("operation_conflict")
                    {
                        (409, "conflict")
                    } else if msg.starts_with("invalid_") {
                        (400, "invalid_request")
                    } else {
                        (500, "internal_error")
                    }
                };
            write_json(stream, status, json!({ "ok": false, "error": safe_msg }))
        }
    }
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
    // /health unknown count excludes acked_unknown rows. See docs/decisions/ack-clear-terminal-unknown.md.
    let outbox_unknown_count = journal.outbox_unknown_unacked_count()?;
    let outbox_dispatching_count =
        journal.outbox_status_count(OutboxDispatchStatus::Dispatching)?;
    let outbox_stale_dispatching_count = journal.outbox_stale_dispatching_count()?;
    let outbox_projection_drift_count = journal.outbox_projection_drift_count()?;
    let worker_job_stale_count = journal.worker_job_stale_count()?;
    let awaiting_approval_count = journal.awaiting_approval_count()?;
    let status = if !hash_chain_ok {
        "corrupt"
    } else if !unknown_invocations.is_empty()
        || outbox_unknown_count > 0
        || outbox_projection_drift_count > 0
        || undelivered_ingress_count > 0
    {
        // `degraded` when the Kernel cannot fully trust its state: live unknown invocations (dispatch
        // started, no terminal receipt); terminal-unknown outbox rows (recovered, never auto-retried,
        // but the dispatch outcome is permanently undetermined); projection drift (projection disagrees
        // with the Journal terminal fact — recovery failed to reconcile); undelivered ingress (accepted
        // but never turned into a worker job / run — transient during startup recovery; persistent
        // non-zero means recovery failed to re-enqueue).
        // Stale counts (outbox_stale_dispatching_count / worker_job_stale_count) are deliberately excluded:
        // they are self-healing transients cleared by the next lease reclaim, not a loss of trust.
        // See docs/decisions/health-rollup-semantics.md (档 C) & docs/decisions/health-rollup-undelivered-ingress.md.
        "degraded"
    } else {
        "ok"
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
        "outbox_projection_drift_count": outbox_projection_drift_count,
        "worker_job_stale_count": worker_job_stale_count,
        "awaiting_approval_count": awaiting_approval_count,
        "unknown_invocation_count": unknown_invocations.len(),
        "unknown_invocations": unknown_invocations.iter().map(|invocation| json!({
            "invocation_id": invocation.invocation_id,
            "run_id": invocation.run_id.as_ref().map(|id| id.0.as_str()),
            "session_id": invocation.session_id.as_ref().map(|id| id.0.as_str()),
            "first_dispatch_at": invocation.first_dispatch_at.to_rfc3339(),
        })).collect::<Vec<_>>(),
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

/// Validate that capability tokens are distinct from each other and from the
/// IPC token. Exits before any listener starts so no HTTP request can observe
/// a collision. Error messages are stable categories, never raw token values.
fn validate_capability_tokens(config: &KernelConfig) -> Result<()> {
    if let (Some(ref sub), Some(ref dec)) = (
        &config.capability_submit_token,
        &config.capability_decision_token,
    ) {
        if sub == dec {
            bail!("capability_token_collision: submit token must differ from decision token");
        }
    }
    if let Some(ref sub) = config.capability_submit_token {
        if sub == &config.ipc_token {
            bail!("capability_token_collision: submit token must differ from IPC token");
        }
    }
    if let Some(ref dec) = config.capability_decision_token {
        if dec == &config.ipc_token {
            bail!("capability_token_collision: decision token must differ from IPC token");
        }
    }
    Ok(())
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}
fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "approval_endpoint_tests.rs"]
mod approval_endpoint_tests;
#[cfg(test)]
#[path = "capability_routes_negative_tests.rs"]
mod capability_routes_negative_tests;
#[cfg(test)]
#[path = "capability_routes_support.rs"]
mod capability_routes_support;
#[cfg(test)]
#[path = "capability_routes_tests.rs"]
mod capability_routes_tests;
#[cfg(test)]
#[path = "harness_endpoint_tests.rs"]
mod harness_endpoint_tests;
