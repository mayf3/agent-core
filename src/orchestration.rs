//! External Orchestration Seam V0 — Kernel side.
//!
//! This module is the *generic* Kernel binding to an external Development
//! Controller. It is deliberately product-agnostic: the Kernel does NOT
//! interpret the raw input, does NOT know any component type or product name,
//! and does NOT parse the controller's output beyond the mechanical checks
//! required to trust a receipt.
//!
//! ## What this seam IS
//! A loopback HTTP call from the Kernel to a configured Controller, carrying
//! an [`ExternalOrchestrationIntent`] (generic governance context + forwarded
//! raw input) and returning an [`ExternalOrchestrationResult`] that the Kernel
//! records as a **single** generic `ReceiptReceived` journal event.
//!
//! ## What this seam is NOT (frozen by the ADR)
//! - NOT a Candidate acceptance authority.
//! - NOT a Capability Proposal trigger.
//! - NOT a Deployment approval.
//! - NOT a Registry activation.
//!
//! Recording the receipt triggers NO Proposal, NO Deployment, and NO Registry
//! mutation. The `ReceiptReceived` event here has exactly the same semantics
//! as every other `ReceiptReceived` in the Kernel: *an already-approved
//! external invocation returned a verifiable result*.
//!
//! See `docs/decisions/external-orchestration-seam-v0.md`.

use crate::domain::{JournalEventKind, ReceiptStatus, RunId, SessionId};
use crate::journal::JournalStore;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub use agent_core_protocol::{
    compute_result_digest, ExternalOrchestrationIntent, ExternalOrchestrationResult, InvocationId,
    OrchestrationOutcome, PrincipalRef, PROTOCOL_VERSION,
};

/// Generic, loopback-only binding to an external Development Controller.
///
/// `url` MUST resolve to loopback (`127.0.0.1` / `localhost`). `token` is
/// optional; when set it is sent as a bearer. The Kernel treats the loopback
/// boundary plus the invocation binding as the trust anchor — the Controller
/// never re-authenticates the principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationBinding {
    pub url: String,
    pub token: Option<String>,
    /// Read/write/connect timeout for the HTTP call, in milliseconds.
    pub timeout_ms: u64,
}

impl OrchestrationBinding {
    /// Build the binding from env, returning `None` when the URL is unset
    /// (the seam is opt-in and disabled by default).
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("AGENT_CORE_EXTERNAL_ORCHESTRATION_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())?;
        let token = std::env::var("AGENT_CORE_EXTERNAL_ORCHESTRATION_TOKEN")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let timeout_ms = std::env::var("AGENT_CORE_EXTERNAL_ORCHESTRATION_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000);
        Some(Self {
            url,
            token,
            timeout_ms,
        })
    }
}

/// Governance context the Kernel already holds when it decides to delegate.
/// This is assembled from the existing Run/Session/Principal state — no new
/// governance objects are minted by the seam.
#[derive(Debug, Clone)]
pub struct OrchestrationContext<'a> {
    pub invocation_id: &'a InvocationId,
    pub run_id: &'a RunId,
    pub session_id: Option<&'a SessionId>,
    pub principal_ref: PrincipalRef,
}

/// Verdict returned when recording a receipt. `Replay` means an identical
/// receipt was already recorded for the same `(invocation_id,
/// idempotency_key)`; `Conflict` means a different receipt was recorded for
/// the same key (rejected). `Appended` means a fresh event was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordVerdict {
    Appended,
    Replay,
    Conflict,
}

/// Mechanical validation of an [`ExternalOrchestrationResult`] against the
/// intent the Kernel issued. Returns the parsed [`OrchestrationOutcome`] on
/// success.
///
/// Checks (all fail-closed):
/// 1. `protocol_version` is the current seam version.
/// 2. `result.invocation_id == intent.invocation_id` (prevents misassignment).
/// 3. `result.result_digest` matches the independently recomputed digest.
pub fn validate_result(
    expected_invocation_id: &InvocationId,
    result: &ExternalOrchestrationResult,
) -> Result<OrchestrationOutcome> {
    if !result.protocol_version.is_current() {
        return Err(anyhow!(
            "orchestration_result_protocol_version_mismatch: expected '{}', got '{}'",
            PROTOCOL_VERSION,
            result.protocol_version.0
        ));
    }
    if result.invocation_id.as_str() != expected_invocation_id.as_str() {
        return Err(anyhow!(
            "orchestration_result_invocation_mismatch: expected '{}', got '{}'",
            expected_invocation_id.as_str(),
            result.invocation_id.as_str()
        ));
    }
    if !result.verify_result_digest() {
        return Err(anyhow!("orchestration_result_digest_mismatch"));
    }
    Ok(result.outcome)
}

/// Build the intent the Kernel sends to the Controller. `raw_input` is
/// forwarded verbatim — the Kernel does not inspect it.
pub fn build_intent(
    ctx: &OrchestrationContext<'_>,
    raw_input: Value,
    idempotency_key: Option<String>,
) -> ExternalOrchestrationIntent {
    ExternalOrchestrationIntent {
        protocol_version: agent_core_protocol::ProtocolVersion::current(),
        invocation_id: ctx.invocation_id.clone(),
        run_id: agent_core_protocol::RunId::new(ctx.run_id.0.as_str()),
        principal_ref: ctx.principal_ref.clone(),
        raw_input,
        context_ref: None,
        idempotency_key,
    }
}

/// Invoke the configured Controller over loopback HTTP. Fails closed on any
/// transport error (connection refused, timeout, non-2xx, malformed body).
pub fn invoke(
    binding: &OrchestrationBinding,
    intent: &ExternalOrchestrationIntent,
) -> Result<ExternalOrchestrationResult> {
    let timeout = Duration::from_millis(binding.timeout_ms.max(100));
    let body = serde_json::to_value(intent)?;
    let response = post_json(&binding.url, binding.token.as_deref(), &body, timeout)?;
    let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        bail!("orchestration_controller_rejected");
    }
    let result_value = response
        .get("result")
        .ok_or_else(|| anyhow!("orchestration_controller_missing_result"))?;
    let result: ExternalOrchestrationResult = serde_json::from_value(result_value.clone())
        .map_err(|e| anyhow!("orchestration_controller_malformed_result: {e}"))?;
    Ok(result)
}

/// Record a verified controller result as a generic `ReceiptReceived` event.
///
/// This reuses the existing [`JournalEventKind::ReceiptReceived`] path — no
/// new table, no new enum variant. The payload carries `invocation_id`,
/// `status` (Succeeded/Failed derived from the outcome), the `result_digest`,
/// and the bounded `outcome`. It does NOT write to capability_proposals,
/// deployment, hcr, component, or registry tables.
///
/// Idempotency: the `correlation_id` is derived from `(invocation_id,
/// idempotency_key)`. The caller may inspect already-recorded events for the
/// same correlation_id to decide between replay and conflict; this helper
/// performs a simple append and returns `Appended`. Higher-level replay /
/// conflict detection is exercised in the tests via
/// [`find_recorded_receipt`].
pub fn record_receipt(
    journal: &JournalStore,
    ctx: &OrchestrationContext<'_>,
    result: &ExternalOrchestrationResult,
    idempotency_key: Option<&str>,
) -> Result<RecordVerdict> {
    let correlation_id = correlation_key(ctx.invocation_id, idempotency_key);
    // If a receipt is already recorded for this correlation_id, classify it
    // as replay (identical result_digest) or conflict (different digest). This
    // is a read-then-append; the Journal's hash-chain makes the append
    // tamper-evident, and duplicate appends are detectable via the
    // correlation_id lookup below.
    if let Some(prev) = find_recorded_receipt(journal, &correlation_id)? {
        if prev.result_digest == result.result_digest {
            return Ok(RecordVerdict::Replay);
        }
        return Ok(RecordVerdict::Conflict);
    }
    let status = match result.outcome {
        OrchestrationOutcome::Succeeded => ReceiptStatus::Succeeded,
        OrchestrationOutcome::Failed => ReceiptStatus::Failed,
    };
    let payload = json!({
        "invocation_id": result.invocation_id.as_str(),
        "status": format!("{:?}", status),
        "outcome": result.outcome.as_str(),
        "result_digest": result.result_digest,
        "evidence_digest": result.evidence_ref.as_ref().map(|r| r.digest.clone()),
        "source": "external_orchestration_seam_v0",
    });
    journal.append_event(
        JournalEventKind::ReceiptReceived,
        Some(ctx.run_id),
        ctx.session_id,
        Some(&correlation_id),
        payload,
    )?;
    Ok(RecordVerdict::Appended)
}

/// A previously-recorded seam receipt, retrieved by correlation_id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedReceipt {
    pub result_digest: String,
    pub outcome: String,
}

/// Look up an already-recorded seam receipt by its correlation_id. Returns
/// `Ok(None)` when none exists. Only considers `ReceiptReceived` events whose
/// payload marks them as originating from this seam (`source ==
/// external_orchestration_seam_v0`), so legacy `ReceiptReceived` events are
/// never misclassified.
///
/// Uses the existing [`JournalStore::events`] scan (no new read API is
/// added to the journal for Seam V0).
pub fn find_recorded_receipt(
    journal: &JournalStore,
    correlation_id: &str,
) -> Result<Option<RecordedReceipt>> {
    for event in journal.events()? {
        if !matches!(event.kind, JournalEventKind::ReceiptReceived) {
            continue;
        }
        let Some(cid) = event.correlation_id.as_deref() else {
            continue;
        };
        if cid != correlation_id {
            continue;
        }
        let source = event
            .payload
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("");
        if source != "external_orchestration_seam_v0" {
            continue;
        }
        let result_digest = event
            .payload
            .get("result_digest")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let outcome = event
            .payload
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Ok(Some(RecordedReceipt {
            result_digest,
            outcome,
        }));
    }
    Ok(None)
}

/// Deterministic correlation key for a seam receipt.
fn correlation_key(invocation_id: &InvocationId, idempotency_key: Option<&str>) -> String {
    match idempotency_key {
        Some(k) => format!(
            "ext-orch:{invocation_id}:{k}",
            invocation_id = invocation_id.as_str()
        ),
        None => format!(
            "ext-orch:{invocation_id}",
            invocation_id = invocation_id.as_str()
        ),
    }
}

// ----- loopback HTTP client (mirrors src/adapters/mod.rs::post_json) -----

fn post_json(url: &str, token: Option<&str>, body: &Value, timeout: Duration) -> Result<Value> {
    let endpoint = Endpoint::parse(url)?;
    let mut stream = connect_with_timeout(&endpoint, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let payload = serde_json::to_string(body)?;
    let auth = match token {
        Some(t) if !t.is_empty() => format!("Authorization: Bearer {t}\r\n"),
        _ => String::new(),
    };
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        endpoint.path,
        endpoint.host,
        auth,
        payload.len(),
        payload
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let Some((head, response_body)) = response.split_once("\r\n\r\n") else {
        bail!("orchestration_controller_malformed_http");
    };
    if !head.starts_with("HTTP/1.1 2") && !head.starts_with("HTTP/1.0 2") {
        bail!("orchestration_controller_http_failed");
    }
    Ok(serde_json::from_str(response_body.trim()).unwrap_or_else(|_| json!({})))
}

fn connect_with_timeout(endpoint: &Endpoint, timeout: Duration) -> Result<TcpStream> {
    let address = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow!("orchestration_controller_unresolved"))?;
    match TcpStream::connect_timeout(&address, timeout) {
        Ok(stream) => Ok(stream),
        Err(e) => Err(anyhow!("orchestration_controller_unavailable: {e}")),
    }
}

#[derive(Debug)]
struct Endpoint {
    host: String,
    port: u16,
    path: String,
}

impl Endpoint {
    fn parse(url: &str) -> Result<Self> {
        let Some(rest) = url.strip_prefix("http://") else {
            bail!("orchestration_controller_url_must_be_http");
        };
        let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = host_port.split_once(':').unwrap_or((host_port, "80"));
        if host != "127.0.0.1" && host != "localhost" {
            bail!("orchestration_controller_url_must_be_loopback");
        }
        Ok(Self {
            host: host.to_string(),
            port: port.parse()?,
            path: format!("/{}", path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RunId, SessionId};
    use crate::journal::JournalStore;

    fn in_memory_journal() -> JournalStore {
        JournalStore::in_memory().expect("in-memory journal")
    }

    /// Collect events that share a correlation_id, using the existing
    /// `events()` scan (Seam V0 adds no new journal read API).
    fn events_for_correlation(
        journal: &JournalStore,
        correlation_id: &str,
    ) -> Result<Vec<crate::domain::JournalEvent>> {
        Ok(journal
            .events()?
            .into_iter()
            .filter(|e| e.correlation_id.as_deref() == Some(correlation_id))
            .collect())
    }

    fn ctx<'a>(invocation_id: &'a InvocationId, run_id: &'a RunId) -> OrchestrationContext<'a> {
        OrchestrationContext {
            invocation_id,
            run_id,
            session_id: None,
            principal_ref: PrincipalRef::new("feishu:ou_test"),
        }
    }

    fn ok_result(invocation_id: &InvocationId) -> ExternalOrchestrationResult {
        ExternalOrchestrationResult::succeeded(
            agent_core_protocol::ProtocolVersion::current(),
            invocation_id.clone(),
            json!({ "echo": "ok" }),
            None,
        )
    }

    // ---- validate_result negative cases ----

    #[test]
    fn validate_result_accepts_well_formed_result() {
        let inv = InvocationId("inv_ok".to_string());
        let result = ok_result(&inv);
        let outcome = validate_result(&inv, &result).expect("ok");
        assert_eq!(outcome, OrchestrationOutcome::Succeeded);
    }

    #[test]
    fn validate_result_rejects_invocation_mismatch() {
        let issued = InvocationId("inv_issued".to_string());
        let mut result = ok_result(&InvocationId("inv_other".to_string()));
        // Force a valid digest for the *other* invocation, then check that the
        // validator still rejects because it does not match the issued one.
        result.result_digest = result.recompute_result_digest();
        let err = validate_result(&issued, &result).expect_err("mismatch");
        assert!(format!("{err}").contains("invocation_mismatch"));
    }

    #[test]
    fn validate_result_rejects_tampered_digest() {
        let inv = InvocationId("inv_tamper".to_string());
        let mut result = ok_result(&inv);
        result.result_digest = "sha256:deadbeef".to_string();
        let err = validate_result(&inv, &result).expect_err("tamper");
        assert!(format!("{err}").contains("digest_mismatch"));
    }

    #[test]
    fn validate_result_rejects_unknown_protocol_version() {
        let inv = InvocationId("inv_pv".to_string());
        let mut result = ok_result(&inv);
        result.protocol_version =
            agent_core_protocol::ProtocolVersion("external-orchestration-v999".into());
        // Re-sign with the bogus version so the only failing check is the PV.
        result.result_digest = result.recompute_result_digest();
        let err = validate_result(&inv, &result).expect_err("pv");
        assert!(format!("{err}").contains("protocol_version_mismatch"));
    }

    // ---- record_receipt idempotency semantics ----

    #[test]
    fn record_receipt_appends_on_first_call() {
        let journal = in_memory_journal();
        let inv = InvocationId("inv_first".to_string());
        let run = RunId("run_first".to_string());
        let result = ok_result(&inv);
        let verdict =
            record_receipt(&journal, &ctx(&inv, &run), &result, Some("key_1")).expect("record");
        assert_eq!(verdict, RecordVerdict::Appended);
        // The event is a generic ReceiptReceived — not a Proposal/Deployment/Registry event.
        let found = find_recorded_receipt(&journal, "ext-orch:inv_first:key_1")
            .expect("lookup")
            .expect("present");
        assert_eq!(found.outcome, "succeeded");
        assert_eq!(found.result_digest, result.result_digest);
    }

    #[test]
    fn record_receipt_replay_returns_same_result() {
        let journal = in_memory_journal();
        let inv = InvocationId("inv_replay".to_string());
        let run = RunId("run_replay".to_string());
        let result = ok_result(&inv);
        let ctx = ctx(&inv, &run);
        let v1 = record_receipt(&journal, &ctx, &result, Some("key_r")).expect("first");
        let v2 = record_receipt(&journal, &ctx, &result, Some("key_r")).expect("second");
        assert_eq!(v1, RecordVerdict::Appended);
        assert_eq!(v2, RecordVerdict::Replay);
        // Only one event was appended.
        let events = events_for_correlation(&journal, "ext-orch:inv_replay:key_r").expect("events");
        let received: Vec<_> = events
            .into_iter()
            .filter(|e| matches!(e.kind, JournalEventKind::ReceiptReceived))
            .collect();
        assert_eq!(
            received.len(),
            1,
            "replay must not append a duplicate event"
        );
    }

    #[test]
    fn record_receipt_conflict_when_digest_differs() {
        let journal = in_memory_journal();
        let inv = InvocationId("inv_conflict".to_string());
        let run = RunId("run_conflict".to_string());
        let ctx = ctx(&inv, &run);
        let first = ok_result(&inv);
        // Same invocation_id, different evidence → different result_digest.
        let mut second = ExternalOrchestrationResult::succeeded(
            agent_core_protocol::ProtocolVersion::current(),
            inv.clone(),
            json!({ "echo": "different" }),
            Some(agent_core_protocol::OpaqueRef::new(
                "evidence",
                "sha256:abcdef",
            )),
        );
        second.result_digest = second.recompute_result_digest();
        let v1 = record_receipt(&journal, &ctx, &first, Some("key_c")).expect("first");
        let v2 = record_receipt(&journal, &ctx, &second, Some("key_c")).expect("second");
        assert_eq!(v1, RecordVerdict::Appended);
        assert_eq!(v2, RecordVerdict::Conflict);
        // Conflict must NOT overwrite or append — only the first receipt stands.
        let events =
            events_for_correlation(&journal, "ext-orch:inv_conflict:key_c").expect("events");
        let received: Vec<_> = events
            .into_iter()
            .filter(|e| matches!(e.kind, JournalEventKind::ReceiptReceived))
            .collect();
        assert_eq!(received.len(), 1, "conflict must not append");
    }

    // ---- fail-closed transport ----

    #[test]
    fn invoke_fails_closed_when_controller_unavailable() {
        // Bind then immediately drop a listener to obtain a refused port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let binding = OrchestrationBinding {
            url: format!("http://127.0.0.1:{port}/v1/orchestrations"),
            token: None,
            timeout_ms: 500,
        };
        let inv = InvocationId("inv_fail".to_string());
        let run = RunId("run_fail".to_string());
        let intent = build_intent(&ctx(&inv, &run), json!({ "text": "x" }), Some("k".into()));
        let err = invoke(&binding, &intent).expect_err("should fail closed");
        assert!(format!("{err}").contains("orchestration_controller"));
    }

    #[test]
    fn endpoint_parser_rejects_non_loopback_url() {
        let err =
            Endpoint::parse("http://10.0.0.1:7500/v1/orchestrations").expect_err("non-loopback");
        assert!(format!("{err}").contains("loopback"));
    }

    #[test]
    fn endpoint_parser_rejects_non_http_url() {
        let err =
            Endpoint::parse("https://127.0.0.1:7500/v1/orchestrations").expect_err("non-http");
        assert!(format!("{err}").contains("http"));
    }

    // ---- end-to-end through a real loopback controller ----

    #[test]
    fn end_to_end_through_loopback_controller() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use std::thread;
        use std::time::Duration;

        // Stand up a tiny inline controller that speaks the seam protocol.
        // This deliberately mirrors what tools/development-controller does,
        // without making the kernel tests depend on the controller crate.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let running = Arc::new(AtomicBool::new(true));
        let run_ctrl = Arc::clone(&running);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblock");
            while run_ctrl.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = handle_one(&mut stream);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        fn handle_one(stream: &mut std::net::TcpStream) -> std::io::Result<()> {
            use std::io::{Read, Write};
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            // Read the HTTP request header-by-byte until we find \r\n\r\n,
            // parse Content-Length from headers, then read exactly that many
            // body bytes — mirroring the kernel server's read_request pattern.
            let mut buf = vec![0u8; 8192];
            let mut used = 0usize;
            loop {
                let n = stream.read(&mut buf[used..])?;
                if n == 0 {
                    break;
                }
                used += n;
                if let Some(hloc) = buf[..used].windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..hloc]);
                    let body_start = hloc + 4;
                    let content_len: usize = head
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.trim().parse().ok())
                        .unwrap_or(0);
                    let total = body_start + content_len;
                    while used < total {
                        let n = stream.read(&mut buf[used..])?;
                        if n == 0 {
                            break;
                        }
                        used += n;
                    }
                    let body = &buf[body_start..body_start + content_len];
                    let intent: agent_core_protocol::ExternalOrchestrationIntent =
                        serde_json::from_slice(body).unwrap_or_else(|_| {
                            panic!(
                                "controller could not parse intent: {:?}",
                                String::from_utf8_lossy(body)
                            )
                        });
                    let result = agent_core_protocol::ExternalOrchestrationResult::succeeded(
                        intent.protocol_version.clone(),
                        intent.invocation_id.clone(),
                        serde_json::json!({ "echo": "ok" }),
                        None,
                    );
                    let resp_body = serde_json::json!({ "ok": true, "result": result });
                    let payload = serde_json::to_string(&resp_body).unwrap();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        payload.len(), payload
                    );
                    stream.write_all(response.as_bytes())?;
                    return Ok(());
                }
            }
            Ok(())
        }

        let binding = OrchestrationBinding {
            url: format!("http://127.0.0.1:{port}/v1/orchestrations"),
            token: None,
            timeout_ms: 2_000,
        };
        let inv = InvocationId("inv_e2e".to_string());
        let run = RunId("run_e2e".to_string());
        let intent = build_intent(
            &ctx(&inv, &run),
            json!({ "text": "hello seam" }),
            Some("e2e".into()),
        );
        let result = invoke(&binding, &intent).expect("invoke");
        let outcome = validate_result(&inv, &result).expect("validate");
        assert_eq!(outcome, OrchestrationOutcome::Succeeded);

        // Record and assert no Proposal/Deployment/Registry side effects.
        let journal = in_memory_journal();
        let session = SessionId("sess_e2e".to_string());
        let ctx_with_session = OrchestrationContext {
            invocation_id: &inv,
            run_id: &run,
            session_id: Some(&session),
            principal_ref: PrincipalRef::new("feishu:ou_e2e"),
        };
        let verdict =
            record_receipt(&journal, &ctx_with_session, &result, Some("e2e")).expect("record");
        assert_eq!(verdict, RecordVerdict::Appended);

        running.store(false, Ordering::SeqCst);
        let _ = handle.join();

        // No capability/deployment/hcr/registry events were written.
        let all = events_for_correlation(&journal, "ext-orch:inv_e2e:e2e").expect("events");
        for e in &all {
            assert!(
                matches!(e.kind, JournalEventKind::ReceiptReceived),
                "seam must only write ReceiptReceived, saw {:?}",
                e.kind
            );
        }
    }
}
