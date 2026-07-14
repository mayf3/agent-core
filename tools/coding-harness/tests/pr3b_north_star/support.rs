use super::helpers;
use agent_core_kernel::config::KernelConfig;
use anyhow::{anyhow, bail, Context, Result};
use coding_harness::config::CodingConfig;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const OWNER_OPEN_ID: &str = "north_star_owner";
pub const OWNER_PRINCIPAL: &str = "feishu:open_id:north_star_owner";
pub const IPC_TOKEN: &str = "pr3b-ipc-token";
pub const DECISION_TOKEN: &str = "pr3b-decision-token";
const HOST_CONTROL_TOKEN: &str = "pr3b-host-control-token-7cb8665fc7a84f1e";
const HOST_EXECUTION_TOKEN: &str = "pr3b-host-execution-token-981a16858d4746b1";
const NETNS_MARKER: &str = "AGENT_CORE_PR3B_ISOLATED_NETNS";

/// Re-exec the one-test binary in a fresh user/network namespace. This keeps
/// fixed production ports 7200/7300 independent from developer services and
/// makes failure to obtain isolation a hard test failure.
pub fn run_in_isolated_network_if_needed() -> Result<bool> {
    if std::env::var(NETNS_MARKER).as_deref() == Ok("1") {
        return Ok(false);
    }
    let executable = std::env::current_exe()?;
    let status = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg("/bin/sh")
        .args(["-c", "ip link set lo up && exec \"$@\"", "sh"])
        .arg(executable)
        .args([
            "--exact",
            "one_sentence_develops_activates_and_executes_calculator",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(NETNS_MARKER, "1")
        .status()
        .context("failed to enter isolated Linux network namespace")?;
    if !status.success() {
        bail!("isolated PR3B North Star child failed: {status}");
    }
    Ok(true)
}

pub fn start_harness(artifact_root: &Path) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:7200")?;
    let config = CodingConfig {
        workspaces: HashMap::new(),
        kernel_api_url: "http://127.0.0.1:0".into(),
        capability_submit_token: "unused".into(),
        artifact_root: artifact_root.to_path_buf(),
        hcr_profiles: HashMap::new(),
        hcr_token: String::new(),
    };
    thread::spawn(move || coding_harness::server::serve(listener, Arc::new(config)));
    Ok(())
}

pub fn start_capability_host() -> Result<()> {
    let config = capability_host::config::CapabilityHostConfig::from_env()
        .map_err(|error| anyhow!(error))?;
    thread::spawn(move || capability_host::server::serve(config));
    Ok(())
}

pub fn start_kernel(
    port: u16,
    db_path: &Path,
    artifact_root: &Path,
    connector_port: u16,
) -> Result<()> {
    let mut config: KernelConfig = helpers::kcfg(&artifact_root.to_path_buf());
    config.db_path = db_path.to_path_buf();
    config.data_dir = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    config.kernel_port = port;
    config.connector_execute_url = format!("http://127.0.0.1:{connector_port}/v1/execute");
    config.ipc_token = IPC_TOKEN.into();
    config.feishu_allowed_open_ids = vec![OWNER_OPEN_ID.into()];
    config.feishu_coding_owner_id = Some(OWNER_OPEN_ID.into());
    config.outbox_dispatcher_enabled = true;
    config.outbox_dispatcher_poll_interval_ms = 10;
    config.capability_decision_token = Some(DECISION_TOKEN.into());
    config.capability_submit_token = Some("pr3b-submit-token".into());
    thread::spawn(move || agent_core_kernel::server::serve(config).expect("Kernel server failed"));
    Ok(())
}

pub fn configure_host_clients(artifact_root: &Path) {
    std::env::set_var("CAPABILITY_HOST_LISTEN_ADDR", "127.0.0.1:7300");
    std::env::set_var("CAPABILITY_HOST_ARTIFACT_ROOT", artifact_root);
    std::env::set_var("CAPABILITY_HOST_CONTROL_TOKEN", HOST_CONTROL_TOKEN);
    std::env::set_var("CAPABILITY_HOST_EXECUTION_TOKEN", HOST_EXECUTION_TOKEN);
    std::env::set_var(
        "AGENT_CORE_CAPABILITY_HOST_CONTROL_URL",
        "http://127.0.0.1:7300",
    );
    std::env::set_var(
        "AGENT_CORE_CAPABILITY_HOST_CONTROL_TOKEN",
        HOST_CONTROL_TOKEN,
    );
    std::env::set_var(
        "AGENT_CORE_CAPABILITY_HOST_EXECUTION_TOKEN",
        HOST_EXECUTION_TOKEN,
    );
}

pub fn feishu_ingress(event_id: &str, message_id: &str, text: &str) -> Value {
    json!({
        "protocol_version": "v1",
        "source": "Feishu",
        "external_event_id": format!("message:{message_id}"),
        "received_at": chrono::Utc::now().to_rfc3339(),
        "payload": {
            "provider_event_id": event_id,
            "sender_open_id": OWNER_OPEN_ID,
            "sender_type": "user",
            "chat_id": "oc_pr3b_owner_p2p",
            "chat_type": "p2p",
            "message_id": message_id,
            "message_type": "text",
            "text": text,
            "mentions": [],
        },
        "routing_hint": {},
    })
}

pub fn pending_proposal_id(message: &Value) -> Option<String> {
    let arguments = message.get("arguments")?;
    let presentation = arguments.get("presentation")?;
    if arguments.get("text").is_some()
        || presentation.get("kind").and_then(Value::as_str)
            != Some("capability_proposal_pending_v1")
    {
        return None;
    }
    presentation
        .get("proposal_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub struct MockFeishuSender {
    pub port: u16,
    messages: Arc<Mutex<Vec<Value>>>,
}

impl MockFeishuSender {
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&messages);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let captured = Arc::clone(&captured);
                thread::spawn(move || {
                    if let Err(error) = handle_connector_request(stream, captured) {
                        eprintln!("mock Feishu sender failed: {error}");
                    }
                });
            }
        });
        Ok(Self { port, messages })
    }

    pub fn messages(&self) -> Vec<Value> {
        self.messages.lock().expect("messages mutex").clone()
    }
}

fn handle_connector_request(mut stream: TcpStream, captured: Arc<Mutex<Vec<Value>>>) -> Result<()> {
    let (request_line, headers, body) = read_http_request(&mut stream)?;
    if request_line != "POST /v1/execute HTTP/1.1"
        || headers.get("authorization").map(String::as_str) != Some(&format!("Bearer {IPC_TOKEN}"))
    {
        return write_http_response(&mut stream, 401, json!({"ok":false}));
    }
    let value: Value = serde_json::from_slice(&body)?;
    if value["protocol_version"] != "v1"
        || value["operation"] != "feishu.send_message"
        || value["invocation_id"].as_str().unwrap_or("").is_empty()
        || value["decision_id"].as_str().unwrap_or("").is_empty()
        || value["idempotency_key"].as_str().unwrap_or("").is_empty()
        || value
            .pointer("/arguments/message_id")
            .and_then(Value::as_str)
            .is_none()
    {
        return write_http_response(&mut stream, 400, json!({"ok":false}));
    }
    let has_text = value
        .pointer("/arguments/text")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty());
    let has_presentation = value
        .pointer("/arguments/presentation/kind")
        .and_then(Value::as_str)
        == Some("capability_proposal_pending_v1");
    if has_text == has_presentation {
        return write_http_response(&mut stream, 400, json!({"ok":false}));
    }
    captured.lock().expect("messages mutex").push(value);
    write_http_response(
        &mut stream,
        200,
        json!({"ok":true,"receipt":{"status":"Succeeded","message_id":"om_mock_reply"}}),
    )
}

fn read_http_request(stream: &mut TcpStream) -> Result<(String, HashMap<String, String>, Vec<u8>)> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let request_line = request_line.trim_end().to_string();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok((request_line, headers, body))
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Value,
}

pub fn http_json(
    method: &str,
    url: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<HttpResponse> {
    let endpoint = url
        .strip_prefix("http://127.0.0.1:")
        .context("test HTTP URL must be loopback")?;
    let (port, path) = endpoint.split_once('/').context("HTTP URL path missing")?;
    let mut stream = TcpStream::connect(("127.0.0.1", port.parse::<u16>()?))?;
    stream.set_read_timeout(Some(Duration::from_secs(300)))?;
    let payload = body
        .map(serde_json::to_vec)
        .transpose()?
        .unwrap_or_default();
    let request = format!(
        "{method} /{path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(&payload)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .context("HTTP response missing body separator")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .context("HTTP response status missing")?;
    let body = serde_json::from_str(body.trim()).unwrap_or_else(|_| json!({}));
    Ok(HttpResponse { status, body })
}

fn write_http_response(stream: &mut TcpStream, status: u16, body: Value) -> Result<()> {
    let payload = serde_json::to_vec(&body)?;
    let reason = if status == 200 { "OK" } else { "Error" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    )?;
    stream.write_all(&payload)?;
    Ok(())
}

pub fn wait_for_health(base: &str, timeout: Duration) -> Result<()> {
    wait_for_value(timeout, || {
        http_json("GET", &format!("{base}/health"), "", None)
            .ok()
            .filter(|response| response.status == 200)
            .map(|_| ())
    })
    .ok_or_else(|| anyhow!("Kernel did not become healthy"))
}

pub fn wait_for_value<T>(timeout: Duration, mut poll: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = poll() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn require_real_linux_sandbox() -> Result<()> {
    let output = Command::new("bwrap")
        .arg("--version")
        .output()
        .context("bubblewrap is required for the PR3B North Star")?;
    if !output.status.success() {
        bail!("bubblewrap --version failed");
    }
    Ok(())
}

pub fn require_fixed_port(port: u16, service: &str) -> Result<()> {
    TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("{service} requires exclusive 127.0.0.1:{port}"))?;
    Ok(())
}

pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

pub fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .with_context(|| format!("missing {field}"))
}

pub fn unique_temp_dir(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()))
}
