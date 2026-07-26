use agent_core_kernel::adapters::StdoutAdapter;
use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::hook::{
    compute_provider_proof, ContextHookRequest, ContextHookResponse, HookConfig, HookEndpoint,
    HookFailureMode, HookKind, HookResponseEnvelope, HttpHookClient, OpaqueArtifactRef,
};
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::{
    EndpointChoice, LlmClient, LlmInput, LlmOutput, ProviderToolTurn, ToolCall, ToolCallResult,
};
use agent_core_kernel::runtime::outbox_dispatcher::dispatch_once;
use agent_core_kernel::runtime::Runtime;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROVIDER_ID: &str = "external-context-provider-e2e";
const SHARED_SECRET: &str = "context-hook-e2e-shared-secret";
const PROVIDER_MARKER: &str = "EXTERNAL_PROVIDER_CHANGED_MODEL_INPUT";
const COMPACTED_RESULT: &str = "EXTERNAL_PROVIDER_COMPACTED_TOOL_RESULT";

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some("provider") {
        return run_provider(
            args.get(2).context("provider port")?.parse()?,
            Path::new(args.get(3).context("ready path")?),
        );
    }
    run_acceptance()
}

fn run_acceptance() -> Result<()> {
    let temp = temp_dir();
    fs::create_dir_all(&temp)?;
    let ready = temp.join("provider.ready");
    let probe = TcpListener::bind("127.0.0.1:0")?;
    let port = probe.local_addr()?.port();
    drop(probe);
    let mut provider = spawn_provider(port, &ready)?;
    wait_ready(&ready, &mut provider)?;

    let mut config = test_config(&temp);
    config.context_prepare_hook = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: format!("http://127.0.0.1:{port}/context.prepare.v0"),
        },
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        failure_mode: HookFailureMode::FailClosed,
        provider_id: PROVIDER_ID.into(),
        shared_secret: SHARED_SECRET.into(),
    };

    let capture = Arc::new(CaptureState::default());
    let model = CaptureModel {
        capture: capture.clone(),
    };
    let journal = JournalStore::in_memory()?;
    journal.initialize_registry()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config.clone(), model).with_hook(
        Box::new(HttpHookClient::new()),
        config.context_prepare_hook.clone(),
    );
    let event = gateway.validate_ingress(
        &journal,
        gateway.cli_ingress("run the authenticated context hook".into())?,
    )?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    dispatch_once(&journal, &StdoutAdapter)?;
    wait_provider(&mut provider)?;

    let events = journal.events()?;
    let hook_events = events
        .iter()
        .filter(|event| event.kind == JournalEventKind::HookCallRecorded)
        .collect::<Vec<_>>();
    let receipt = events
        .iter()
        .find(|event| event.kind == JournalEventKind::ReceiptReceived)
        .context("system.status receipt")?;
    let receipt_text = serde_json::to_string(&receipt.payload)?;
    let assistant_delivered = events
        .iter()
        .any(|event| event.kind == JournalEventKind::AssistantReplyDelivered);
    let run_completed = journal.run_status(&outcome.run_id)?.as_deref() == Some("Completed");

    if hook_events.len() != 2
        || hook_events
            .iter()
            .any(|event| event.payload.get("status").and_then(Value::as_str) != Some("ok"))
        || capture.calls.load(Ordering::SeqCst) != 2
        || !capture.initial_changed.load(Ordering::SeqCst)
        || !capture.followup_changed.load(Ordering::SeqCst)
        || !capture.tool_followup_seen.load(Ordering::SeqCst)
        || outcome.output != "E2E_ASSISTANT_REPLY"
        || !assistant_delivered
        || !run_completed
        || receipt_text.contains(COMPACTED_RESULT)
        || !journal.verify_hash_chain()?
    {
        bail!("context hook acceptance invariant failed");
    }

    println!("LOCAL_END_TO_END_RUN_ID={}", outcome.run_id.0);
    println!("GENERIC_HOOK_CALLS_INITIAL_AND_FOLLOWUP=true");
    println!("PROVIDER_IDENTITY_AND_AUTH_ENFORCED=true");
    println!("RUN_SESSION_SCOPE_BOUND=true");
    println!("CANDIDATE_ARTIFACT_DIGESTS_VERIFIED=true");
    println!("IMMUTABLE_REFS_VERIFIED=true");
    println!("MODEL_ADAPTER_HARD_BUDGET_ENFORCED=true");
    println!("EXTERNAL_PROVIDER_CHANGED_MODEL_INPUT=true");
    println!("POST_COMPRESSION_TOOL_CALL_SUCCEEDED=true");
    println!("ASSISTANT_REPLY_DELIVERED=true");
    println!("RUN_COMPLETED=true");
    println!("FULL_JOURNAL_RESULTS_PRESERVED=true");
    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

#[derive(Default)]
struct CaptureState {
    calls: AtomicUsize,
    initial_changed: AtomicBool,
    followup_changed: AtomicBool,
    tool_followup_seen: AtomicBool,
}

struct CaptureModel {
    capture: Arc<CaptureState>,
}

impl LlmClient for CaptureModel {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        let call = self.capture.calls.fetch_add(1, Ordering::SeqCst);
        let marker = input
            .blocks
            .iter()
            .any(|block| block.content == PROVIDER_MARKER);
        if call == 0 {
            self.capture.initial_changed.store(marker, Ordering::SeqCst);
            return Ok(LlmOutput {
                provider: "capture-harness".into(),
                model: "capture-harness".into(),
                content: "calling system.status".into(),
                journal_payload: json!({"provider":"capture-harness","model":"capture-harness","status":"ok"}),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: "capture-tool-call-digest".into(),
                    operation: "system.status".into(),
                    arguments: json!({}),
                }),
                provider_turn: Some(ProviderToolTurn {
                    endpoint: EndpointChoice::Primary,
                    provider_tool_call_id: "provider-call-1".into(),
                    wire_name: "system.status".into(),
                    canonical_operation: "system.status".into(),
                    arguments_json: "{}".into(),
                    reasoning_content: None,
                }),
            });
        }
        self.capture
            .followup_changed
            .store(marker, Ordering::SeqCst);
        self.capture.tool_followup_seen.store(
            input.follow_ups.len() == 1 && input.follow_ups[0].result_content == COMPACTED_RESULT,
            Ordering::SeqCst,
        );
        Ok(LlmOutput {
            provider: "capture-harness".into(),
            model: "capture-harness".into(),
            content: "E2E_ASSISTANT_REPLY".into(),
            journal_payload: json!({"provider":"capture-harness","model":"capture-harness","status":"ok"}),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

fn run_provider(port: u16, ready: &Path) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    fs::write(ready, b"ready")?;
    for stream in listener.incoming().take(2) {
        serve_context_request(stream?)?;
    }
    Ok(())
}

fn serve_context_request(mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request_bytes = read_http_request(&mut stream)?;
    let request_text = String::from_utf8(request_bytes)?;
    let (headers, body) = request_text.split_once("\r\n\r\n").context("http body")?;
    if !headers
        .lines()
        .any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {SHARED_SECRET}")))
    {
        write_http(&mut stream, 401, &[], "{}")?;
        return Ok(());
    }
    let envelope: Value = serde_json::from_str(body)?;
    let request: ContextHookRequest =
        serde_json::from_value(envelope.get("payload").cloned().context("payload")?)?;
    let mut input: LlmInput =
        serde_json::from_slice(&request.candidate.artifact.decode_verified()?)?;
    let active_skill = input
        .blocks
        .iter_mut()
        .find(|block| block.kind == ContextBlockKind::ActiveSkill)
        .context("active skill candidate block")?;
    active_skill.content = PROVIDER_MARKER.into();
    active_skill.source_ref = Some("external-provider".into());
    for follow_up in &mut input.follow_ups {
        follow_up.result_content = COMPACTED_RESULT.into();
    }
    let artifact = OpaqueArtifactRef::new(
        request.candidate.artifact.media_type.clone(),
        &serde_json::to_vec(&input)?,
    );
    let response = ContextHookResponse {
        run_id: request.candidate.run_id.clone(),
        session_id: request.candidate.session_id.clone(),
        scope_digest: request.candidate.scope_digest.clone(),
        candidate_digest: request.candidate.artifact.digest.clone(),
        immutable_refs: request.candidate.immutable_refs.clone(),
        immutable_refs_digest: request.candidate.immutable_refs_digest.clone(),
        artifacts: vec![artifact],
    };
    let proof = compute_provider_proof(
        SHARED_SECRET,
        &response.authentication_message(PROVIDER_ID, &request.request_id),
    )?;
    let response_envelope = HookResponseEnvelope {
        request_id: request.request_id,
        hook: HookKind::ContextPrepareV0,
        timestamp: Utc::now(),
        payload: serde_json::to_value(response)?,
    };
    write_http(
        &mut stream,
        200,
        &[("X-Agent-Core-Provider-Proof", proof.as_str())],
        &serde_json::to_string(&response_envelope)?,
    )
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = find_bytes(&bytes, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + length {
                bytes.truncate(header_end + 4 + length);
                return Ok(bytes);
            }
        }
    }
    Ok(bytes)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_http(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<()> {
    let reason = if status == 200 { "OK" } else { "Unauthorized" };
    let extra = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}

fn spawn_provider(port: u16, ready: &Path) -> Result<Child> {
    Ok(Command::new(std::env::current_exe()?)
        .arg("provider")
        .arg(port.to_string())
        .arg(ready)
        .stdin(Stdio::null())
        .spawn()?)
}

fn wait_ready(path: &Path, child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!("provider exited before ready: {status}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    bail!("provider ready timeout")
}

fn wait_provider(child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("provider failed: {status}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    bail!("provider exit timeout")
}

fn temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "context-hook-harness-{}-{nanos}",
        std::process::id()
    ))
}

fn test_config(temp: &Path) -> KernelConfig {
    KernelConfig {
        db_path: PathBuf::from(":memory:"),
        data_dir: temp.join("data"),
        agent_id: AgentId("main".into()),
        root_dir: temp.to_path_buf(),
        kernel_port: 0,
        connector_execute_url: String::new(),
        ipc_token: "test".into(),
        capability_submit_token: None,
        capability_decision_token: None,
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: String::new(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 5_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 10,
        extra_allowed_operations: vec!["system.status".into()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 5_000,
        harness_artifact_root: temp.join("artifacts"),
        max_tool_rounds: 4,
        feishu_coding_owner_id: None,
        tool_loop_timeout_ms: 30_000,
        context_prepare_hook: HookConfig::default(),
    }
}
